"""Durable inference operations over SeeCut's existing account wallet."""
from __future__ import annotations

import json
import hashlib

from .assistant_provider import make_assistant_provider, AssistantProvider, PreparedInference
from .assistant_models import model_config
from dataclasses import replace
from copy import copy, deepcopy
from .efforts import FROZEN_FIELDS, public_efforts, resolve_effort, selection_pair, validate_efforts

from . import assistant_contract as contract
from .assistant_billing import capture_for_usage, credit_cost
from .errors import ApiError
from .security import new_id

SETTLED = frozenset({'captured', 'released'})
TERMINAL = frozenset({'succeeded', 'failed', 'cancelled', 'outcome_unknown'})


class AssistantService:
    def __init__(self, account, provider, *, config=None, model_id="default", label=None, providers=None):
        self.account = account
        self.db = account.db
        self.config = config if config is not None else account.config
        self.now = account.now
        self.provider = provider
        self.model_id = model_id
        self.label = label or self.config.assistant_model[:64] or '账户模型'
        self.effort_policy = None
        self.bound_budget_id = None
        self.budget_route = None
        self.selection_applied = False
        self.models = {}
        if config is None:
            validate_efforts(self.config)
        if model_id == 'default':
            for specification in self.config.assistant_models:
                child_config = model_config(self.config, specification)
                child_provider = (providers or {}).get(specification['id'])
                if child_provider is None:
                    child_provider = make_assistant_provider(child_config, self.db)
                self.models[specification['id']] = AssistantService(
                    account, child_provider, config=child_config,
                    model_id=specification['id'], label=specification['label'])

    def _model_service(self, model_id, *, read_only=False):
        contract.identity(model_id)
        if model_id == self.model_id:
            return self
        service = self.models.get(model_id)
        if service is not None:
            return service
        if read_only:
            # Removed aliases remain queryable/cancellable. They cannot dispatch
            # through the current default model or reconcile with another provider.
            config = replace(self.config, assistant_model='', assistant_models=())
            return AssistantService(self.account, AssistantProvider(config), config=config, model_id=model_id)
        raise ApiError(409, 'ASSISTANT_MODEL_UNAVAILABLE', '所选 Agent 模型当前不可用')

    def _with_policy(self, policy, *, budget_id=None, route=None):
        # Routing/credentials remain owned by the current provider. Only the
        # original public limits and rates are reconstructed from this budget.
        fields = {'assistant_' + name: policy['agent'][name] for name in FROZEN_FIELDS}
        fields['assistant_rate_denominator'] = policy['agent'].get('rate_denominator', 1000)
        config = replace(self.config, **fields, assistant_models=(), efforts=())
        provider = self.provider
        if hasattr(provider, 'config'):
            provider = copy(provider)
            provider.config = config
        frozen = AssistantService(self.account, provider, config=config,
                                  model_id=self.model_id, label=self.label)
        frozen.effort_policy = deepcopy(policy)
        frozen.bound_budget_id = budget_id
        frozen.budget_route = route
        frozen.selection_applied = True
        return frozen

    def _selected(self, body, user, *, creating_budget=False):
        if not isinstance(body, dict):
            contract.invalid()
        if self.selection_applied:
            return self, body
        if 'policy' in body or ('effort_id' in body) != ('policy_revision' in body):
            raise ApiError(400, 'EFFORT_POLICY_INVALID', '请仅提交完整的强度策略标识')
        pair = selection_pair(body.get('effort_id'), body.get('policy_revision'))
        model_id = body.get('model_id', self.model_id)
        contract.identity(model_id)
        row = None
        with self.db.connect() as connection:
            if creating_budget and all(isinstance(body.get(name), str) for name in ('local_session_id', 'turn_id')):
                row = connection.execute('SELECT * FROM assistant_turn_budgets WHERE user_id=? AND local_session_id=? AND turn_id=?',
                                         (user, body['local_session_id'], body['turn_id'])).fetchone()
            elif not creating_budget and isinstance(body.get('budget_id'), str):
                row = connection.execute('SELECT * FROM assistant_turn_budgets WHERE user_id=? AND id=?',
                                         (user, body['budget_id'])).fetchone()
        if row is not None and row['policy_json'] is not None:
            if pair != (row['effort_id'], row['policy_revision']) or model_id != row['model_id']:
                raise ApiError(409, 'EFFORT_POLICY_MISMATCH', '请求与原回合强度策略不一致')
            policy = json.loads(row['policy_json'])
            service = self._model_service(model_id, read_only=True)._with_policy(
                policy, budget_id=row['id'], route=row['route_fingerprint'])
        elif row is not None:
            # A legacy budget stays legacy even after the deployment adds tiers.
            if pair is not None:
                raise ApiError(409, 'EFFORT_POLICY_MISMATCH', '原回合未绑定强度策略')
            service = self._model_service(model_id)
        else:
            policy = resolve_effort(self.account, body.get('effort_id'), body.get('policy_revision'))
            if policy is not None and model_id != policy['agent']['model_id']:
                raise ApiError(409, 'EFFORT_POLICY_MISMATCH', 'Agent 模型与所选强度策略不一致')
            service = self._model_service(model_id)
            if policy is not None:
                service = service._with_policy(policy)
        return service, {key: value for key, value in body.items() if key not in {'model_id', 'effort_id', 'policy_revision'}}

    def _selection_digest(self, body):
        selection = {} if self.model_id == 'default' else {'model_id': self.model_id}
        if self.effort_policy is not None:
            selection.update(effort_id=self.effort_policy['id'], policy_revision=self.effort_policy['policy_revision'])
        return contract.digest(body | selection)

    def _budget_service(self, user, budget_id, *, read_only=False):
        if self.bound_budget_id == budget_id:
            return self
        with self.db.connect() as connection:
            row = connection.execute('SELECT * FROM assistant_turn_budgets WHERE id=? AND user_id=?',
                                     (budget_id, user)).fetchone()
        if row is None:
            raise ApiError(404, 'ASSISTANT_BUDGET_NOT_FOUND', 'Agent 本轮预算不存在')
        service = self._model_service(row['model_id'], read_only=read_only)
        if row['policy_json'] is not None:
            service = service._with_policy(json.loads(row['policy_json']), budget_id=budget_id, route=row['route_fingerprint'])
        return service

    def _operation_service(self, user, operation_id, *, read_only=False):
        with self.db.connect() as connection:
            row = self._owned(connection, user, operation_id)
        return self._budget_service(user, row['budget_id'], read_only=read_only)

    def execution_service(self, operation):
        return self._budget_service(operation['user_id'], operation['budget_id'], read_only=True)

    def capabilities(self, *, include_models=True):
        c = self.config
        available = bool(self.provider.configured and self.input_contract_ready() and c.assistant_pricing_version and
                         c.assistant_input_rate > 0 and c.assistant_output_rate > 0 and
                         0 < c.assistant_max_output_tokens <= c.assistant_context_window and
                         0 < c.assistant_max_steps <= 8 and c.assistant_max_turn_credits > 0)
        value = dict(available=available, reason_code=None if available else ('ASSISTANT_INPUT_CONTRACT_UNAVAILABLE' if not self.input_contract_ready() else 'ASSISTANT_NOT_CONFIGURED'),
                    protocol_version=1, images=c.assistant_images, tools=True,
                    context_window=c.assistant_context_window, max_output_tokens=c.assistant_max_output_tokens,
                    max_steps=c.assistant_max_steps, max_turn_credits=c.assistant_max_turn_credits,
                    pricing_version=c.assistant_pricing_version, rate_denominator=c.assistant_rate_denominator)
        value.update(model_id=self.model_id, label=self.label)
        if include_models:
            value['default_model_id'] = self.model_id
            value['models'] = [value.copy()] + [child.capabilities(include_models=False) for child in self.models.values()]
            value.update(public_efforts(self.account))
        return value

    def input_contract_ready(self):
        identity = getattr(self.provider, 'token_contract_id', None)
        return bool(isinstance(identity, str) and contract.IDENTITY.fullmatch(identity)
                    and callable(getattr(self.provider, 'prepare', None)) and not self.config.assistant_images)

    def prepare(self, request, *, user=None, budget_id=None, client_request_id=None):
        if self.budget_route is not None and self.budget_route != self.route_fingerprint():
            raise ApiError(409, 'EFFORT_POLICY_MISMATCH', '原回合模型路由已变更，不能重新绑定')
        self._available()
        try:
            scoped = getattr(self.provider, "prepare_scoped", None)
            prepared = scoped(request, dict(user_id=user, budget_id=budget_id, client_request_id=client_request_id)) if callable(scoped) else self.provider.prepare(request)
        except Exception:
            raise ApiError(503, 'ASSISTANT_INPUT_CONTRACT_UNAVAILABLE', 'Agent 输入计数规则尚未验证') from None
        if not isinstance(prepared, PreparedInference) or type(prepared.input_token_upper_bound) is not int or \
                prepared.input_token_upper_bound < 0 or prepared.token_contract_id != self.provider.token_contract_id or \
                not isinstance(prepared.payload, bytes) or not 0 < len(prepared.payload) <= 2 * 1024 * 1024:
            raise ApiError(503, 'ASSISTANT_INPUT_CONTRACT_UNAVAILABLE', 'Agent 输入计数规则无效')
        if prepared.input_token_upper_bound + request['maxOutputTokens'] > self.config.assistant_context_window:
            raise ApiError(400, 'ASSISTANT_CONTEXT_EXCEEDED', 'Agent 输入与输出总量超过本轮上下文限制')
        return prepared

    def route_fingerprint(self):
        return contract.digest({'protocol': 'openai-chat-v1', 'base': self.config.assistant_base_url,
                                'model': self.config.assistant_model, 'token_contract': getattr(self.provider, 'token_contract_id', None)})

    def _available(self):
        if not self.capabilities(include_models=False)['available']:
            raise ApiError(503, 'ASSISTANT_NOT_CONFIGURED', 'Agent 服务尚未配置')

    @staticmethod
    def _conflict():
        raise ApiError(409, 'ASSISTANT_IDEMPOTENCY_CONFLICT', '该请求标识已用于其他内容')

    def _replay(self, connection, user, namespace, key, digest):
        contract.identity(key)
        row = connection.execute('SELECT * FROM assistant_idempotency WHERE user_id=? AND namespace=? AND key=?',
                                 (user, namespace, key)).fetchone()
        if row and row['digest'] != digest:
            self._conflict()
        return row['target_id'] if row else None

    @staticmethod
    def _remember(connection, user, namespace, key, digest, target):
        connection.execute('INSERT OR IGNORE INTO assistant_idempotency VALUES(?,?,?,?,?)',
                           (user, namespace, key, digest, target))

    @staticmethod
    def _budget_snapshot(row):
        value = {('budget_id' if key == 'id' else key): row[key] for key in (
            'id', 'local_session_id', 'turn_id', 'max_credits', 'held_credits', 'spent_credits',
            'operation_count', 'max_steps', 'state', 'expires_at', 'model_id', 'effort_id', 'policy_revision',
            'rounding_mode', 'accrued_microcredits')}
        value['policy'] = json.loads(row['policy_json']) if row['policy_json'] is not None else None
        return value

    def _budget(self, connection, user, budget_id, scope=None):
        row = connection.execute('SELECT * FROM assistant_turn_budgets WHERE id=? AND user_id=?',
                                 (budget_id, user)).fetchone()
        if not row:
            raise ApiError(404, 'ASSISTANT_BUDGET_NOT_FOUND', 'Agent 本轮预算不存在')
        if row['model_id'] != self.model_id:
            raise ApiError(409, 'ASSISTANT_SCOPE_MISMATCH', 'Agent 模型与本轮预算不一致')
        expected = contract.canonical(self.effort_policy) if self.effort_policy is not None else None
        if row['policy_json'] != expected:
            raise ApiError(409, 'EFFORT_POLICY_MISMATCH', '请求与原回合强度策略不一致')
        if scope and (row['local_session_id'] != scope['local_session_id'] or row['turn_id'] != scope['turn_id']):
            raise ApiError(409, 'ASSISTANT_SCOPE_MISMATCH', 'Agent 请求与本轮预算不一致')
        return row

    def turn_budget(self, user, body, key):
        service, body = self._selected(body, user, creating_budget=True)
        if service is not self:
            return service.turn_budget(user, body, key)
        body = dict(body)
        expected_policy_digest = body.pop('expected_policy_digest', None)
        contract.fields(body, {'client_request_id', 'local_session_id', 'turn_id'})
        for value in body.values():
            contract.identity(value)
        digest = self._selection_digest(body)
        with self.db.transaction(immediate=True) as connection:
            target = self._replay(connection, user, 'budget', key, digest)
            # The body identity and header are independently idempotent.
            original = self._replay(connection, user, 'budget-request', body['client_request_id'], digest)
            row = connection.execute('SELECT * FROM assistant_turn_budgets WHERE user_id=? AND local_session_id=? AND turn_id=?',
                                     (user, body['local_session_id'], body['turn_id'])).fetchone()
            if not row:
                # Compare the accepted selection with exactly the server policy
                # about to be persisted, before any write in this transaction.
                # Existing/legacy budgets recover their original record instead.
                if self.effort_policy is not None:
                    if expected_policy_digest is None:
                        raise ApiError(409, 'EFFORT_POLICY_DIGEST_REQUIRED', '请提交已冻结的强度策略摘要')
                    if (not isinstance(expected_policy_digest, str) or len(expected_policy_digest) != 64
                            or any(char not in '0123456789abcdef' for char in expected_policy_digest)):
                        raise ApiError(400, 'EFFORT_POLICY_INVALID', '冻结的强度策略摘要无效')
                    if expected_policy_digest != contract.digest(self.effort_policy):
                        raise ApiError(409, 'EFFORT_POLICY_STALE', '强度策略已变更，请重新选择')
                elif expected_policy_digest is not None:
                    raise ApiError(400, 'EFFORT_POLICY_INVALID', '原回合未绑定强度策略')
                self._available()
                # A client-chosen session/turn cannot evade an unfinished account turn.
                connection.execute("UPDATE assistant_turn_budgets SET state='expired' WHERE user_id=? AND state='active' AND held_credits=0 AND operation_count=0 AND expires_at<=?", (user, self.now()))
                if connection.execute("SELECT 1 FROM assistant_turn_budgets WHERE user_id=? AND state='active'", (user,)).fetchone():
                    raise ApiError(409, 'ASSISTANT_ACTIVE_TURN', '请先结束或核实当前 Agent 回合')
                target = new_id('abud')
                connection.execute('''INSERT INTO assistant_turn_budgets
                    (id,user_id,local_session_id,turn_id,max_credits,max_steps,created_at,expires_at,model_id,
                     effort_id,policy_revision,policy_json,route_fingerprint,rounding_mode,accrued_microcredits) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,'task_cumulative_v1',0)''',
                    (target, user, body['local_session_id'], body['turn_id'], self.config.assistant_max_turn_credits,
                     self.config.assistant_max_steps, self.now(), self.now() + 120, self.model_id,
                     self.effort_policy['id'] if self.effort_policy else None,
                     self.effort_policy['policy_revision'] if self.effort_policy else None,
                     contract.canonical(self.effort_policy) if self.effort_policy else None,
                     self.route_fingerprint() if self.effort_policy else None))
                row = self._budget(connection, user, target)
            row = self._budget(connection, user, row['id'])
            if (target and target != row['id']) or (original and original != row['id']):
                self._conflict()
            self._remember(connection, user, 'budget', key, digest, row['id'])
            self._remember(connection, user, 'budget-request', body['client_request_id'], digest, row['id'])
            return self._budget_snapshot(row)

    def close_budget(self, user, budget_id, body, key):
        service = self._budget_service(user, budget_id, read_only=True)
        if service is not self:
            return service.close_budget(user, budget_id, body, key)
        contract.fields(body, set())
        with self.db.transaction(immediate=True) as connection:
            row = self._budget(connection, user, budget_id)
            digest = contract.digest({'budget_id': budget_id})
            self._replay(connection, user, 'budget-close', key, digest)
            unresolved = connection.execute("""SELECT 1 FROM assistant_operations WHERE budget_id=? AND
                (state NOT IN ('succeeded','failed','cancelled') OR billing_state NOT IN ('captured','released')) LIMIT 1""",
                (budget_id,)).fetchone()
            if row['held_credits'] or unresolved:
                raise ApiError(409, 'ASSISTANT_UNRESOLVED_OPERATION', '请先核实原 Agent 操作')
            self._remember(connection, user, 'budget-close', key, digest, budget_id)
            connection.execute("UPDATE assistant_turn_budgets SET state='closed' WHERE id=?", (budget_id,))
            return self._budget_snapshot(self._budget(connection, user, budget_id))

    def _check_budget(self, row, credits):
        if row['state'] != 'active' or (row['operation_count'] == 0 and row['expires_at'] <= self.now()) or row['operation_count'] >= row['max_steps'] or \
                row['spent_credits'] + row['held_credits'] + credits > row['max_credits']:
            raise ApiError(409, 'ASSISTANT_BUDGET_EXCEEDED', '本轮 Agent 积分或步骤已达到上限')

    def quote(self, user, body, key):
        service, body = self._selected(body, user)
        if service is not self:
            return service.quote(user, body, key)
        normalized = contract.inference_body(body, self.config)
        digest = self._selection_digest(normalized)
        with self.db.transaction(immediate=True) as connection:
            replay = self._replay(connection, user, 'quote', key, digest)
            original = self._replay(connection, user, 'quote-request', body['client_request_id'], digest)
            target = original or replay
            row = connection.execute('SELECT * FROM assistant_quotes WHERE id=? AND user_id=?', (target, user)).fetchone() if target else None
            accepted = connection.execute('SELECT 1 FROM assistant_operations WHERE user_id=? AND client_request_id=?',
                                          (user, body['client_request_id'])).fetchone()
            if accepted and not row:
                self._conflict()
            if not accepted:
                self._available()
                budget = self._budget(connection, user, body['budget_id'], body)
                # A delayed first quote or confirmation may extend only this same
                # active, unused budget. Closed/replaced budgets never revive.
                if budget['state'] == 'active' and budget['operation_count'] == 0 and not budget['held_credits'] and not budget['spent_credits']:
                    if connection.execute('SELECT 1 FROM assistant_operations WHERE budget_id=?', (budget['id'],)).fetchone():
                        self._conflict()
                    connection.execute('UPDATE assistant_turn_budgets SET expires_at=MAX(expires_at,?) WHERE id=?', (self.now() + 120, budget['id']))
                    budget = self._budget(connection, user, budget['id'])
                self._check_budget(budget, 0)
            if row and (accepted or row['expires_at'] > self.now()):
                self._remember(connection, user, 'quote', key, digest, row['id'])
            else:
                prepared = self.prepare(body['request'], user=user, budget_id=body['budget_id'], client_request_id=body['client_request_id'])
                credits = credit_cost(self.config.assistant_context_window, body['request']['maxOutputTokens'],
                                      self.config.assistant_input_rate, self.config.assistant_output_rate,
                                      self.config.assistant_rate_denominator)
                self._check_budget(budget, credits)
                target = new_id('aquo')
                connection.execute('''INSERT INTO assistant_quotes
                    (id,user_id,request_digest,pricing_version,route_fingerprint,input_rate,output_rate,context_window,
                     input_token_upper_bound,payload_digest,max_credits,expires_at,created_at,rate_denominator) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)''',
                    (target, user, digest, self.config.assistant_pricing_version, self.route_fingerprint(), self.config.assistant_input_rate,
                     self.config.assistant_output_rate, self.config.assistant_context_window, prepared.input_token_upper_bound,
                     hashlib.sha256(prepared.payload).hexdigest(), credits, self.now() + 120, self.now(), self.config.assistant_rate_denominator))
                # Move every alias to the one refreshed quote in the same transaction.
                connection.execute("UPDATE assistant_idempotency SET target_id=? WHERE user_id=? AND namespace IN ('quote','quote-request') AND digest=?", (target, user, digest))
                self._remember(connection, user, 'quote', key, digest, target)
                self._remember(connection, user, 'quote-request', body['client_request_id'], digest, target)
                row = connection.execute('SELECT * FROM assistant_quotes WHERE id=?', (target,)).fetchone()
            return dict(quote_id=row['id'], request_digest=row['request_digest'], pricing_version=row['pricing_version'],
                        max_credits=row['max_credits'], expires_at=row['expires_at'],
                        effort_id=self.effort_policy['id'] if self.effort_policy else None,
                        policy_revision=self.effort_policy['policy_revision'] if self.effort_policy else None,
                        policy=deepcopy(self.effort_policy))

    def _owned(self, connection, user, operation_id):
        row = connection.execute('SELECT * FROM assistant_operations WHERE id=? AND user_id=?', (operation_id, user)).fetchone()
        if not row:
            raise ApiError(404, 'ASSISTANT_OPERATION_NOT_FOUND', 'Agent 操作不存在')
        return row

    def _snapshot(self, connection, row):
        value = {('operation_id' if key == 'id' else key): row[key] for key in (
            'id', 'client_request_id', 'local_session_id', 'turn_id', 'step_id', 'budget_id', 'request_digest',
            'state', 'version', 'last_event_seq', 'billing_state', 'reserved_credits', 'captured_credits', 'released_credits')}
        value['cancel_requested'] = bool(row['cancel_requested'])
        for key in ('result', 'error', 'usage'):
            value[key] = json.loads(row[key + '_json']) if row[key + '_json'] is not None else None
        value['budget'] = self._budget_snapshot(self._budget(connection, row['user_id'], row['budget_id']))
        return value

    def create(self, user, body, key):
        service, body = self._selected(body, user)
        if service is not self:
            return service.create(user, body, key)
        normalized = contract.inference_body(body, self.config, creation=True)
        digest = self._selection_digest(normalized)
        with self.db.transaction(immediate=True) as connection:
            target = self._replay(connection, user, 'create', key, digest)
            row = connection.execute('SELECT * FROM assistant_operations WHERE user_id=? AND client_request_id=?',
                                     (user, body['client_request_id'])).fetchone()
            if row:
                if row['request_digest'] != digest or (target and target != row['id']):
                    self._conflict()
                self._remember(connection, user, 'create', key, digest, row['id'])
                return self._snapshot(connection, row), False
            if target:
                self._conflict()
            self._available()
            # An unresolved original blocks subsequent inference even in a new turn/budget.
            blocked = connection.execute('''SELECT 1 FROM assistant_operations WHERE user_id=?
                AND (state NOT IN ('succeeded','failed','cancelled') OR billing_state NOT IN ('captured','released')) LIMIT 1''',
                (user,)).fetchone()
            if blocked:
                raise ApiError(409, 'ASSISTANT_UNRESOLVED_OPERATION', '请先核实原 Agent 操作')
            if connection.execute('SELECT 1 FROM assistant_operations WHERE user_id=? AND local_session_id=? AND turn_id=? AND step_id=?',
                                  (user, body['local_session_id'], body['turn_id'], body['step_id'])).fetchone():
                self._conflict()
            budget = self._budget(connection, user, body['budget_id'], body)
            quote = connection.execute('SELECT * FROM assistant_quotes WHERE id=? AND user_id=?', (body['quote_id'], user)).fetchone()
            if not quote or quote['request_digest'] != digest or quote['consumed_at'] is not None or \
                    quote['expires_at'] <= self.now() or quote['pricing_version'] != self.config.assistant_pricing_version or \
                    quote['route_fingerprint'] != self.route_fingerprint() or quote['input_rate'] != self.config.assistant_input_rate or \
                    quote['output_rate'] != self.config.assistant_output_rate or quote['context_window'] != self.config.assistant_context_window or \
                    quote['rate_denominator'] != self.config.assistant_rate_denominator:
                raise ApiError(409, 'ASSISTANT_QUOTE_MISMATCH', 'Agent 报价已过期或与请求不一致')
            prepared = self.prepare(body['request'], user=user, budget_id=body['budget_id'], client_request_id=body['client_request_id'])
            if prepared.input_token_upper_bound != quote['input_token_upper_bound'] or hashlib.sha256(prepared.payload).hexdigest() != quote['payload_digest']:
                raise ApiError(409, 'ASSISTANT_QUOTE_MISMATCH', 'Agent 输入规则已变更')
            credits = quote['max_credits']
            self._check_budget(budget, credits)
            wallet = connection.execute('SELECT * FROM wallets WHERE user_id=?', (user,)).fetchone()
            if not wallet or wallet['available_credits'] < credits:
                raise ApiError(409, 'INSUFFICIENT_CREDITS', '积分不足，请先充值')
            target = new_id('aop')
            connection.execute('''INSERT INTO assistant_operations
                (id,user_id,client_request_id,local_session_id,turn_id,step_id,budget_id,request_digest,request_json,
                 pricing_version,input_rate,output_rate,route_fingerprint,context_window,input_token_upper_bound,payload_digest,rate_denominator,state,reserved_credits,created_at,updated_at)
                VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?, 'queued',?,?,?)''',
                (target, user, body['client_request_id'], body['local_session_id'], body['turn_id'], body['step_id'],
                 budget['id'], digest, contract.canonical(body['request']), quote['pricing_version'],
                 quote['input_rate'], quote['output_rate'], quote['route_fingerprint'], quote['context_window'], quote['input_token_upper_bound'], quote['payload_digest'], quote['rate_denominator'], credits, self.now(), self.now()))
            self._remember(connection, user, 'create', key, digest, target)
            connection.execute('UPDATE assistant_quotes SET consumed_at=? WHERE id=?', (self.now(), quote['id']))
            connection.execute('UPDATE assistant_turn_budgets SET held_credits=held_credits+?,operation_count=operation_count+1 WHERE id=?',
                               (credits, budget['id']))
            connection.execute('UPDATE wallets SET available_credits=available_credits-?,held_credits=held_credits+?,updated_at=? WHERE user_id=?',
                               (credits, credits, self.now(), user))
            connection.execute("INSERT INTO wallet_holds VALUES(?,?,?,?,'held',?,?)",
                               (new_id('hold'), user, 'assistant:' + target, credits, self.now(), self.now()))
            self._ledger(connection, user, target, 'hold', -credits, credits)
            self._event(connection, target, 'accepted', {'state': 'queued'})
            return self._snapshot(connection, self._owned(connection, user, target)), True

    def get(self, user, operation_id):
        service = self._operation_service(user, operation_id, read_only=True)
        if service is not self:
            return service.get(user, operation_id)
        with self.db.connect() as connection:
            return self._snapshot(connection, self._owned(connection, user, operation_id))

    def by_request(self, user, request_id):
        contract.identity(request_id)
        with self.db.connect() as connection:
            row = connection.execute('SELECT * FROM assistant_operations WHERE user_id=? AND client_request_id=?', (user, request_id)).fetchone()
            if not row:
                raise ApiError(404, 'ASSISTANT_OPERATION_NOT_FOUND', 'Agent 操作不存在')
            return self.get(user, row['id'])

    def events(self, user, operation_id, after):
        service = self._operation_service(user, operation_id, read_only=True)
        if service is not self:
            return service.events(user, operation_id, after)
        contract.integer(after, 0, 1_000_000)
        with self.db.transaction() as connection:
            row = self._owned(connection, user, operation_id)
            if after > row['last_event_seq']:
                raise ApiError(409, 'ASSISTANT_INVALID_CURSOR', 'Agent 事件游标无效')
            def gap():
                raise ApiError(409, 'ASSISTANT_CURSOR_EXPIRED', 'Agent 事件记录不完整，请核实原操作',
                               {'snapshot': self._snapshot(connection, row)})
            events, size, expected = [], 0, after + 1
            for event in connection.execute('SELECT * FROM assistant_events WHERE operation_id=? AND seq>? ORDER BY seq LIMIT 128', (operation_id, after)):
                if event['seq'] != expected:
                    gap()
                expected += 1
                item = dict(operation_id=operation_id, seq=event['seq'], type=event['type'],
                            payload=json.loads(event['payload_json']), created_at=event['created_at'])
                count = len(contract.canonical(item).encode())
                if events and size + count > 262144:
                    break
                events.append(item)
                size += count
            if not events and after < row['last_event_seq']:
                gap()
            cursor = events[-1]['seq'] if events else after
            return dict(operation_id=operation_id, events=events, next_cursor=cursor,
                        has_more=cursor < row['last_event_seq'], state=row['state'], billing_state=row['billing_state'])

    def _event(self, connection, operation_id, kind, payload):
        seq = connection.execute('SELECT last_event_seq FROM assistant_operations WHERE id=?', (operation_id,)).fetchone()[0] + 1
        connection.execute('INSERT INTO assistant_events VALUES(?,?,?,?,?)',
                           (operation_id, seq, kind, contract.canonical(payload), self.now()))
        connection.execute('UPDATE assistant_operations SET last_event_seq=?,version=version+1,updated_at=? WHERE id=?',
                           (seq, self.now(), operation_id))

    def _ledger(self, connection, user, operation, kind, available, held):
        connection.execute('''INSERT INTO ledger_entries
            (id,user_id,kind,delta_available,delta_held,reference_type,reference_id,idempotency_key,created_at)
            VALUES(?,?,?,?,?,'assistant_operation',?,?,?)''',
            (new_id('led'), user, kind, available, held, operation, f'assistant:{operation}:{kind}', self.now()))

    def cancel(self, user, operation_id, body, key):
        service = self._operation_service(user, operation_id, read_only=True)
        if service is not self:
            return service.cancel(user, operation_id, body, key)
        contract.fields(body, set())
        with self.db.transaction(immediate=True) as connection:
            row = self._owned(connection, user, operation_id)
            digest = contract.digest({'operation_id': operation_id})
            self._replay(connection, user, 'cancel', key, digest)
            self._remember(connection, user, 'cancel', key, digest, operation_id)
            if not row['cancel_requested']:
                connection.execute('UPDATE assistant_operations SET cancel_requested=1 WHERE id=?', (operation_id,))
                self._event(connection, operation_id, 'state.changed', {'state': row['state'], 'cancel_requested': True})
                if row['billing_state'] in SETTLED:
                    latest = connection.execute('SELECT id FROM assistant_operations WHERE budget_id=? ORDER BY rowid DESC LIMIT 1', (row['budget_id'],)).fetchone()
                    if latest['id'] == operation_id:
                        connection.execute("UPDATE assistant_turn_budgets SET state='closed' WHERE id=? AND held_credits=0", (row['budget_id'],))
                if row['state'] == 'queued':
                    self._settle(connection, row, 'cancelled', {'input': 0, 'output': 0})
            return self._snapshot(connection, self._owned(connection, user, operation_id))

    def claim(self):
        with self.db.transaction(immediate=True) as connection:
            row = connection.execute("SELECT * FROM assistant_operations WHERE state='queued' ORDER BY created_at,id LIMIT 1").fetchone()
            if row is None:
                return None
            connection.execute("UPDATE assistant_operations SET state='dispatching' WHERE id=?", (row['id'],))
            self._event(connection, row['id'], 'state.changed', {'state': 'dispatching'})
            return dict(connection.execute('SELECT * FROM assistant_operations WHERE id=?', (row['id'],)).fetchone())

    def cancelled(self, operation_id):
        with self.db.connect() as connection:
            return bool(connection.execute('SELECT cancel_requested FROM assistant_operations WHERE id=?', (operation_id,)).fetchone()[0])

    def append_model(self, operation_id, event):
        contract.model_event(event)
        if event['type'] == 'done':
            raise ValueError('done is only published in settlement transaction')
        with self.db.transaction(immediate=True) as connection:
            row = connection.execute('SELECT * FROM assistant_operations WHERE id=?', (operation_id,)).fetchone()
            if row['state'] not in {'dispatching', 'running'}:
                raise ValueError('operation is no longer executing')
            connection.execute("UPDATE assistant_operations SET state='running' WHERE id=?", (operation_id,))
            self._event(connection, operation_id, 'model', event)

    def finish(self, operation_id, state, usage=None, result=None, error=None):
        with self.db.transaction(immediate=True) as connection:
            row = connection.execute('SELECT * FROM assistant_operations WHERE id=?', (operation_id,)).fetchone()
            self._finish_in_transaction(connection, row, state, usage, result, error)

    @staticmethod
    def _validate_finish(row, state, usage, result):
        if state not in {'succeeded', 'failed', 'cancelled'}:
            raise ValueError('invalid terminal execution state')
        if result is not None:
            contract.model_event(result)
            if result['type'] != 'done':
                raise ValueError('invalid final event')
        if usage is not None:
            contract.usage(usage)
        if row['billing_state'] in SETTLED:
            return
        if result is not None:
            allowed_tools = {tool['name'] for tool in json.loads(row['request_json'])['tools']}
            if any(call['name'] not in allowed_tools for call in result['message'].get('toolCalls', [])):
                raise ValueError('unrequested tool call')
        if state == 'succeeded' and result is None:
            raise ValueError('successful execution needs a complete result')

    def _finish_in_transaction(self, connection, row, state, usage, result=None, error=None):
        self._validate_finish(row, state, usage, result)
        if row['billing_state'] not in SETTLED:
            self._settle(connection, row, state, usage, result, error)

    def _settle(self, connection, row, state, usage, result=None, error=None):
        if row['billing_state'] in SETTLED:
            return
        cost = None
        capture = None
        budget = connection.execute('SELECT * FROM assistant_turn_budgets WHERE id=? AND user_id=?',
                                    (row['budget_id'], row['user_id'])).fetchone()
        if budget is None:
            raise RuntimeError('assistant budget missing')
        limits_exceeded = usage is not None and (usage['input'] > row['input_token_upper_bound'] or usage['input'] + usage['output'] > row['context_window'] or
                           usage['output'] > json.loads(row['request_json'])['maxOutputTokens'])
        if usage is not None and not limits_exceeded:
            capture = capture_for_usage(row, budget, usage)
            cost = capture.credits
        if cost is None or cost > row['reserved_credits'] or limits_exceeded:
            connection.execute("""UPDATE assistant_operations SET state=?,billing_state='settlement_pending',result_json=?,usage_json=?,error_json=? WHERE id=?""",
                               (state, contract.canonical(result) if result else None, contract.canonical(usage) if usage else None,
                                contract.canonical({'code': 'ASSISTANT_USAGE_UNCONFIRMED', 'message': 'Agent 用量待核实'}), row['id']))
            self._event(connection, row['id'], 'billing.changed', {'billing_state': 'settlement_pending'})
            return
        release = row['reserved_credits'] - cost
        hold = connection.execute('SELECT * FROM wallet_holds WHERE task_id=?', ('assistant:' + row['id'],)).fetchone()
        if not hold or hold['state'] != 'held' or hold['credits'] != row['reserved_credits']:
            raise RuntimeError('assistant wallet hold inconsistent')
        billing = 'captured' if cost else 'released'
        connection.execute('UPDATE wallets SET available_credits=available_credits+?,held_credits=held_credits-?,updated_at=? WHERE user_id=?',
                           (release, row['reserved_credits'], self.now(), row['user_id']))
        connection.execute('UPDATE wallet_holds SET state=?,updated_at=? WHERE id=?', (billing, self.now(), hold['id']))
        connection.execute('UPDATE assistant_turn_budgets SET held_credits=held_credits-?,spent_credits=spent_credits+?,accrued_microcredits=? WHERE id=?',
                           (row['reserved_credits'], cost, capture.accrued_microcredits, row['budget_id']))
        if state != 'succeeded' or row['cancel_requested']:
            connection.execute("UPDATE assistant_turn_budgets SET state='closed' WHERE id=?", (row['budget_id'],))
        if cost:
            self._ledger(connection, row['user_id'], row['id'], 'capture', 0, -cost)
        if release:
            self._ledger(connection, row['user_id'], row['id'], 'release', release, -release)
        connection.execute('''UPDATE assistant_operations SET state=?,billing_state=?,captured_credits=?,released_credits=?,
            result_json=?,usage_json=?,error_json=? WHERE id=?''',
            (state, billing, cost, release, contract.canonical(result) if result else None,
             contract.canonical(usage), contract.canonical(error) if error else None, row['id']))
        self._event(connection, row['id'], 'usage.confirmed', usage)
        self._event(connection, row['id'], 'billing.changed', dict(billing_state=billing, captured_credits=cost, released_credits=release))
        if state == 'succeeded':
            self._event(connection, row['id'], 'model', result)
        self._event(connection, row['id'], 'terminal', {'state': state, 'billing_state': billing})

    def unknown(self, operation_id):
        with self.db.transaction(immediate=True) as connection:
            row = connection.execute('SELECT * FROM assistant_operations WHERE id=?', (operation_id,)).fetchone()
            if row['billing_state'] in SETTLED or row['state'] == 'outcome_unknown':
                return
            connection.execute("UPDATE assistant_operations SET state='outcome_unknown',error_json=? WHERE id=?",
                               (contract.canonical({'code': 'ASSISTANT_OUTCOME_UNKNOWN', 'message': 'Agent 原操作结果待核实'}), operation_id))
            self._event(connection, operation_id, 'terminal', {'state': 'outcome_unknown', 'billing_state': row['billing_state']})

    def recover_interrupted(self):
        # Called once under the execution lock, before the worker dispatches.
        with self.db.connect() as connection:
            ids = [row[0] for row in connection.execute("SELECT id FROM assistant_operations WHERE state IN ('dispatching','running')")]
        for operation_id in ids:
            self.unknown(operation_id)
        return len(ids)

    def reconcile(self, user, operation_id, body, key):
        service = self._operation_service(user, operation_id)
        if service is not self:
            return service.reconcile(user, operation_id, body, key)
        contract.fields(body, set())
        with self.db.transaction(immediate=True) as connection:
            row = self._owned(connection, user, operation_id)
            digest = contract.digest({'operation_id': operation_id})
            self._replay(connection, user, 'reconcile', key, digest)
            self._remember(connection, user, 'reconcile', key, digest, operation_id)
            if row['billing_state'] in SETTLED:
                return self._snapshot(connection, row)
            needs_lookup = row['state'] == 'outcome_unknown' or row['billing_state'] == 'settlement_pending'
        if needs_lookup and row['route_fingerprint'] == self.route_fingerprint():
            try:
                outcome = self.provider.lookup(operation_id)
                if outcome is not None:
                    self.finish(operation_id, **outcome)
            except Exception:
                # A failed status read proves neither failure nor absence of execution.
                pass
        return self.get(user, operation_id)
