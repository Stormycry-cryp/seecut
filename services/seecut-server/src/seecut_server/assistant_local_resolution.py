"""Offline, single-operation accounting with explicit local operator declarations."""
from __future__ import annotations

import argparse
import json
import re
import sqlite3
import time
from pathlib import Path

from . import assistant_contract as contract
from .assistant_execution_lock import execution_lock
from .assistant_service import AssistantService, SETTLED
from .db import Database
from .errors import ApiError


class _Accounting(AssistantService):
    """Use only the existing connection-local settlement methods; no provider/config."""
    def __init__(self, database, now):
        self.db = database
        self.now = now


def _normalize(body):
    contract.fields(body, {'user_id', 'operation_id', 'expected_version', 'resolution_id', 'operator', 'reason'},
                    {'evidence_reference', 'evidence_confirmed', 'outcome', 'terminal_state', 'usage', 'result'})
    value = {name: contract.identity(body[name]) for name in ('user_id', 'operation_id', 'resolution_id')}
    value['expected_version'] = contract.integer(body['expected_version'], 1, 2**63 - 1)
    for name, limit in (('operator', 128), ('reason', 512)):
        text = contract.text(body[name], limit).strip()
        if not text or any(ord(char) < 32 for char in text):
            contract.invalid('请提供简短的本地操作者和核账理由，不提交日志')
        value[name] = text
    evidence = body.get('evidence_reference')
    if evidence is not None:
        contract.fields(evidence, {'id', 'sha256'})
        reference = contract.identity(evidence['id'])
        if not re.fullmatch(r'[A-Za-z][A-Za-z0-9_.-]{0,31}:[A-Za-z0-9_.:-]{1,95}', reference):
            contract.invalid('证据只接受非敏感类别:标识和摘要，不接受网址或日志')
        if reference.split(':', 1)[0].lower() in {'key', 'secret', 'token', 'password', 'credential', 'bearer'}:
            contract.invalid('证据引用不能包含凭据')
        digest = evidence['sha256']
        if not isinstance(digest, str) or not re.fullmatch(r'[a-fA-F0-9]{64}', digest):
            contract.invalid('请提供证据 SHA256 摘要')
        evidence = {'id': reference, 'sha256': digest.lower()}
    value['evidence_reference'] = evidence
    confirmed = body.get('evidence_confirmed', False)
    if type(confirmed) is not bool:
        contract.invalid()
    value['evidence_confirmed'] = confirmed
    outcome = body.get('outcome', 'unknown')
    if outcome not in ('unknown', 'confirmed_not_executed', 'confirmed_usage'):
        contract.invalid()
    value['outcome'] = outcome
    state, usage, result = body.get('terminal_state'), body.get('usage'), body.get('result')
    if state is not None and state not in ('failed', 'cancelled', 'succeeded'):
        contract.invalid()
    if usage is not None:
        contract.usage(usage)
    if result is not None:
        try:
            contract.model_event(result)
        except ApiError as exc:
            raise ApiError(400, 'ASSISTANT_RESOLUTION_RESULT_INVALID', 'done 未通过既有契约') from exc
        if result['type'] != 'done':
            raise ApiError(400, 'ASSISTANT_RESOLUTION_RESULT_INVALID', '必须提供完整 done 结果')
    if outcome == 'confirmed_not_executed':
        if state not in (None, 'failed') or usage not in (None, {'input': 0, 'output': 0}) or result is not None:
            contract.invalid('确认未执行只能按失败和零用量收尾')
        state, usage = 'failed', {'input': 0, 'output': 0}
    if result is not None and 'usage' in result and result['usage'] != usage:
        raise ApiError(400, 'ASSISTANT_RESOLUTION_RESULT_INVALID', 'done 与声明用量不一致')
    value.update(terminal_state=state, usage=usage, result=result)
    contract.canonical(value)
    return value


class LocalResolution:
    def __init__(self, database, *, clock=time.time):
        self.db = database
        self.now = lambda: int(clock())
        self.accounting = _Accounting(database, self.now)

    def _prior_evidence(self, row):
        try:
            usage = contract.usage(json.loads(row['usage_json'])) if row['usage_json'] is not None else None
            result = json.loads(row['result_json']) if row['result_json'] is not None else None
            if row['result_json'] is not None:
                self.accounting._validate_finish(row, 'succeeded', usage, result)
                if result is None:
                    raise ValueError('stored result is not done')
                if 'usage' in result:
                    if usage is not None and result['usage'] != usage:
                        raise ValueError('stored evidence disagrees')
                    usage = result['usage']
            return usage, result
        except (ApiError, ValueError, TypeError, KeyError, RecursionError) as exc:
            raise ApiError(409, 'ASSISTANT_RESOLUTION_PRIOR_EVIDENCE_INVALID',
                           '仍待核实：已有证据未通过契约，请另行核实，不能按零费用处理') from exc

    @staticmethod
    def _check_prior_evidence(value, usage, result):
        if usage is not None and usage != value['usage']:
            raise ApiError(409, 'ASSISTANT_RESOLUTION_EVIDENCE_CONFLICT', '人工用量与已保存的有效用量矛盾')
        if result is not None and (value['outcome'] != 'confirmed_usage' or value['terminal_state'] != 'succeeded'
                                   or contract.canonical(result) != contract.canonical(value['result'])):
            raise ApiError(409, 'ASSISTANT_RESOLUTION_EVIDENCE_CONFLICT', '人工结果不能否定或覆盖已保存的完整 done 结果')

    def _summary(self, connection, row):
        budget = connection.execute('SELECT * FROM assistant_turn_budgets WHERE id=? AND user_id=?',
                                    (row['budget_id'], row['user_id'])).fetchone()
        if not budget or (budget['local_session_id'], budget['turn_id']) != (row['local_session_id'], row['turn_id']):
            raise ApiError(409, 'ASSISTANT_RESOLUTION_SCOPE_CONFLICT', '原操作预算归属不一致')
        contract.identity(budget['model_id'])
        if budget['policy_json'] is not None and budget['route_fingerprint'] != row['route_fingerprint']:
            raise ApiError(409, 'ASSISTANT_RESOLUTION_SCOPE_CONFLICT', '原操作模型路由不一致')
        if min(row['input_rate'], row['output_rate'], row['input_token_upper_bound']) < 0 or row['context_window'] <= 0 or row['reserved_credits'] <= 0:
            raise ApiError(409, 'ASSISTANT_RESOLUTION_SCOPE_CONFLICT', '原冻结价格或限额无效')
        wallet = connection.execute('SELECT available_credits,held_credits FROM wallets WHERE user_id=?',
                                    (row['user_id'],)).fetchone()
        hold = connection.execute('SELECT user_id,credits,state FROM wallet_holds WHERE task_id=?',
                                  ('assistant:' + row['id'],)).fetchone()
        if not wallet or not hold or hold['user_id'] != row['user_id'] or hold['credits'] != row['reserved_credits']:
            raise ApiError(409, 'ASSISTANT_RESOLUTION_SCOPE_CONFLICT', '原冻结记录不一致')
        value = {name: row[name] for name in ('user_id', 'version', 'state', 'billing_state', 'reserved_credits',
                                             'captured_credits', 'released_credits', 'pricing_version', 'input_rate', 'output_rate')}
        value.update(operation_id=row['id'], model_id=budget['model_id'], wallet=dict(wallet), hold=dict(hold),
                     budget={name: budget[name] for name in ('id', 'model_id', 'held_credits', 'spent_credits', 'state')})
        return value

    def _resolve(self, connection, value, *, apply, acquired):
        digest = contract.digest(value)
        has_audit = connection.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='assistant_resolutions'").fetchone()
        old = connection.execute('SELECT * FROM assistant_resolutions WHERE resolution_id=?',
                                 (value['resolution_id'],)).fetchone() if has_audit else None
        if old is not None:
            if old['input_digest'] != digest:
                raise ApiError(409, 'ASSISTANT_RESOLUTION_ID_CONFLICT', '核账标识已被其他输入使用')
            return json.loads(old['result_json'])
        row = self.accounting._owned(connection, value['user_id'], value['operation_id'])
        before = self._summary(connection, row)
        if row['version'] != value['expected_version']:
            raise ApiError(409, 'ASSISTANT_RESOLUTION_VERSION_CONFLICT', '原操作版本已变化')
        if row['billing_state'] in SETTLED or not (row['state'] == 'outcome_unknown' or row['billing_state'] == 'settlement_pending'):
            raise ApiError(409, 'ASSISTANT_RESOLUTION_NOT_PENDING', '原操作不是待核实的未结算操作')
        try:
            prior_usage, prior_result = self._prior_evidence(row)
        except ApiError as exc:
            if apply:
                raise
            return dict(status='unresolved', applied=False, reason='prior_evidence_invalid', message=exc.message, before=before)
        complete = value['outcome'] != 'unknown' and value['evidence_confirmed'] and value['evidence_reference'] is not None
        complete = complete and value['terminal_state'] is not None and value['usage'] is not None
        if complete:
            try:
                self.accounting._validate_finish(row, value['terminal_state'], value['usage'], value['result'])
                self._check_prior_evidence(value, prior_usage, prior_result)
            except ValueError as exc:
                raise ApiError(400, 'ASSISTANT_RESOLUTION_RESULT_INVALID', '收尾结果未通过既有契约') from exc
        if not complete:
            if apply:
                raise ApiError(409, 'ASSISTANT_RESOLUTION_EVIDENCE_REQUIRED', '仍待核实：需要人工证据声明、引用和明确结果')
            return dict(status='unresolved', applied=False, reason='evidence_unconfirmed', before=before)
        usage = value['usage']
        cost = (usage['input'] * row['input_rate'] + usage['output'] * row['output_rate'] + 999) // 1000
        limits = usage['input'] > row['input_token_upper_bound'] or usage['input'] + usage['output'] > row['context_window'] or usage['output'] > json.loads(row['request_json'])['maxOutputTokens']
        if not apply:
            status = 'ready' if acquired is True and not limits and cost <= row['reserved_credits'] else 'unresolved'
            return dict(status=status, applied=False, before=before,
                        proposed=dict(state=value['terminal_state'], cost=cost, limits_exceeded=limits,
                                      within_reservation=cost <= row['reserved_credits']), executor_stopped=acquired is True)
        if not has_audit:
            raise ApiError(409, 'ASSISTANT_RESOLUTION_MIGRATION_REQUIRED', '核账审计表尚未安装')
        error = None if value['terminal_state'] == 'succeeded' else {
            'code': 'ASSISTANT_MANUALLY_RESOLVED', 'message': 'Agent 操作已按人工证据收尾'}
        self.accounting._finish_in_transaction(connection, row, value['terminal_state'], usage, value['result'], error)
        after = self._summary(connection, self.accounting._owned(connection, value['user_id'], value['operation_id']))
        result = dict(resolution_id=value['resolution_id'], status='resolved' if after['billing_state'] in SETTLED else 'unresolved',
                      applied=True, operator_authentication='local_declaration', before=before, after=after)
        summary = {key: data for key, data in value.items() if key != 'result'}
        summary['result_digest'] = contract.digest(value['result']) if value['result'] is not None else None
        connection.execute('''INSERT INTO assistant_resolutions
            (resolution_id,input_digest,user_id,operation_id,operator,operator_authentication,reason,
             evidence_reference_json,input_summary_json,before_json,after_json,result_json,created_at)
            VALUES(?,?,?,?,?,'local_declaration',?,?,?,?,?,?,?)''',
            (value['resolution_id'], digest, value['user_id'], value['operation_id'], value['operator'], value['reason'],
             contract.canonical(value['evidence_reference']), contract.canonical(summary), contract.canonical(before),
             contract.canonical(after), contract.canonical(result), self.now()))
        return result

    def resolve(self, body, *, apply=False):
        value = _normalize(body)
        with execution_lock(self.db.path, required=apply, create=apply) as acquired:
            if apply and acquired is not True:
                raise ApiError(409, 'ASSISTANT_EXECUTOR_ACTIVE', '执行者尚未停止，请稍后核实')
            if apply:
                with self.db.transaction(immediate=True) as connection:
                    return self._resolve(connection, value, apply=True, acquired=True)
            # Read-only even when the caller supplied a writable Database object.
            readonly = Database(self.db.path, self.db.schema_path, read_only=True)
            with readonly.transaction() as connection:
                return self._resolve(connection, value, apply=False, acquired=acquired)


def main(argv=None):
    parser = argparse.ArgumentParser(description='服务器本地单操作核账；默认只读。操作者仅为本地声明。')
    parser.add_argument('--database', type=Path, required=True)
    for name in ('user-id', 'operation-id', 'resolution-id', 'operator', 'reason'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--expected-version', type=int, required=True)
    parser.add_argument('--outcome', choices=('unknown', 'confirmed_not_executed', 'confirmed_usage'), default='unknown')
    parser.add_argument('--terminal-state', choices=('failed', 'cancelled', 'succeeded'))
    parser.add_argument('--input-tokens', type=int)
    parser.add_argument('--output-tokens', type=int)
    parser.add_argument('--result-file', type=Path, help='完整 done JSON；不是日志文件')
    parser.add_argument('--evidence-reference', help='非敏感类别:标识，不提供密钥、网址或日志')
    parser.add_argument('--evidence-sha256')
    parser.add_argument('--confirm-evidence', action='store_true', help='声明已人工核验证据，确认该结果和真实用量')
    parser.add_argument('--apply', action='store_true', help='明确写入；请先审阅 dry-run')
    args = parser.parse_args(argv)
    try:
        path = args.database.resolve(strict=True)
        if not path.is_file():
            raise ValueError('existing local database required')
        body = {name: getattr(args, name) for name in ('user_id', 'operation_id', 'expected_version', 'resolution_id', 'operator', 'reason', 'outcome', 'terminal_state')}
        body['evidence_confirmed'] = args.confirm_evidence
        if args.evidence_reference is not None or args.evidence_sha256 is not None:
            body['evidence_reference'] = dict(id=args.evidence_reference, sha256=args.evidence_sha256)
        if args.input_tokens is not None or args.output_tokens is not None:
            body['usage'] = dict(input=args.input_tokens, output=args.output_tokens)
        if args.result_file is not None:
            with args.result_file.open('rb') as handle:
                data = handle.read(196609)
            if len(data) > 196608:
                raise ValueError('done result too large')
            body['result'] = json.loads(data)
        database = Database(path, Path(__file__).resolve().parents[2] / 'schema.sql', read_only=not args.apply)
        result = LocalResolution(database).resolve(body, apply=args.apply)
        print(contract.canonical(dict(mode='apply' if args.apply else 'dry_run', result=result)))
        return 3 if args.apply and result['status'] == 'unresolved' else 0
    except ApiError as exc:
        print(contract.canonical(dict(status='unresolved', code=exc.code, message=exc.message)))
    except (OSError, ValueError, sqlite3.Error):
        print(contract.canonical(dict(status='unresolved', code='ASSISTANT_LOCAL_RESOLUTION_FAILED',
                                      message='本地核账未完成，请保留记录并核实输入及数据库')))
    return 2


if __name__ == '__main__':
    raise SystemExit(main())
