import copy
import os
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from seecut_server.config import Config
from seecut_server.db import Database
from seecut_server.errors import ApiError
from seecut_server.prompt_assistance import PromptAssistance, JEV_MODEL, MODEL, DOMESTIC_BASE, rank


class PromptAssistanceTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        with patch.dict(os.environ, {'SECUT_ENV': 'test'}, clear=True):
            config = Config.from_env()
        self.config = replace(config, beatapi_api_key='synthetic', assistant_api_key='synthetic', assistant_model=MODEL, assistant_base_url=DOMESTIC_BASE)
        self.db = Database(Path(self.temp.name)/'db', Path(__file__).resolve().parents[1]/'schema.sql'); self.db.initialize()
        self.now = 1000
        self.calls = []
        self.reply = {'model': JEV_MODEL, 'answers': {'best_template': {'type': 'choice', 'choice': 't_1', 'probabilities': {'t_0': .1, 't_1': .7, 't_2': .2}}}, 'usage': {'input_tokens': 100, 'output_tokens': 0}}
        self.service = SimpleNamespace(config=self.config, db=self.db, clock=lambda:self.now)
        self.api = PromptAssistance(self.service, send=self.send)
        self.body = {'prompt': '保留主体，换成柔光背景', 'images': [], 'templates': [{'id':str(i), 'name':name, 'prompt':'以参考主体为准，'+name+'，保持细节。'} for i,name in enumerate(['白底棚拍','自然光','材质特写'])], 'recentPrompts': []}

    def send(self, *args):
        self.calls.append(args)
        return copy.deepcopy(self.reply)

    def run_call(self, key='request-00000001', body=None, op='recommend', user='one'):
        return self.api.run(user, op, key, body or self.body)

    def test_selection_uses_only_given_prompts_and_no_wallet(self):
        with self.db.connect() as db: before = list(db.execute('SELECT * FROM wallets'))
        result = self.run_call()
        self.assertEqual([x['id'] for x in result['recommendations']], ['1','2','0'])
        self.assertEqual(result['userCredits'], 0)
        self.assertEqual(self.calls[0][2]['questions']['best_template']['criteria']['t_0'], self.body['templates'][0]['prompt'])
        with self.db.connect() as db: self.assertEqual(list(db.execute('SELECT * FROM wallets')), before)

    def test_replay_is_persistent_and_changed_intent_rejected(self):
        expected = self.run_call()
        self.api = PromptAssistance(self.service, send=self.send)
        self.assertEqual(self.run_call(), expected)
        self.assertEqual(len(self.calls), 1)
        with self.assertRaises(ApiError) as error:
            self.run_call(body={**self.body, 'prompt':'另一个要求'})
        self.assertEqual(error.exception.code, 'PROMPT_CONFLICT')

    def test_client_history_cannot_override_account_history(self):
        self.run_call(body={**self.body, 'recentPrompts':['untrusted other account']})
        self.assertEqual(self.calls[0][2]['state']['recentPrompts'], [])

    def test_account_isolation_and_shared_refresh_interval(self):
        self.run_call()
        with self.assertRaises(ApiError) as error:
            self.run_call('request-00000002')
        self.assertEqual(error.exception.status, 429)
        self.run_call(user='two')
        self.now += 10; self.run_call('request-00000002')
        self.assertEqual(len(self.calls), 3)

    def test_unknown_result_never_resubmitted(self):
        self.api.send = lambda *args: (_ for _ in ()).throw(TimeoutError())
        with self.assertRaises(TimeoutError): self.run_call()
        self.api.send = self.send
        self.now += 100
        with self.assertRaises(ApiError) as error: self.run_call()
        self.assertEqual(error.exception.code, 'PROMPT_UNKNOWN'); self.assertEqual(self.calls, [])

    def test_rejects_unknown_missing_nonfinite_probabilities(self):
        for changes in [{'extra': .1}, {'t_0': float('nan')}, {'t_0': -1}]:
            reply = copy.deepcopy(self.reply); reply['answers']['best_template']['probabilities'].update(changes)
            with self.assertRaises(ApiError): rank(reply, self.body['templates'])
        reply = copy.deepcopy(self.reply); del reply['answers']['best_template']['probabilities']['t_2']
        with self.assertRaises(ApiError): rank(reply, self.body['templates'])

    def test_rewrite_uses_glm_and_preserves_template_length(self):
        self.reply = {'model': MODEL, 'choices':[{'finish_reason':'stop', 'message':{'content':'以参考主体为准，柔光白底棚拍，保持细节。'}}], 'usage':{'prompt_tokens':60,'completion_tokens':20}}
        result = self.run_call(op='rewrite', body={**self.body,'templateId':'0'})
        self.assertEqual(result['model'], MODEL)
        self.assertEqual(self.calls[0][0], DOMESTIC_BASE+'/chat/completions')
        self.assertEqual(self.calls[0][2]['thinking']['type'], 'enabled')
        self.assertNotIn('image_url', str(self.calls[0][2]))
        self.assertEqual(self.body['prompt'], '保留主体，换成柔光背景')

    def test_truncated_or_expanded_rewrite_is_not_applied(self):
        for reason, content in [('length','short'),('stop','long'*100)]:
            self.reply = {'model':MODEL,'choices':[{'finish_reason':reason,'message':{'content':content}}]}
            with self.assertRaises(ApiError): self.run_call(key='request-'+reason+'-00001', op='rewrite', body={**self.body,'templateId':'0'})
            self.now += 3

    def test_no_credentials_or_external_image_url(self):
        self.service.config = replace(self.config, beatapi_api_key='')
        with self.assertRaises(ApiError) as error: self.run_call()
        self.assertEqual(error.exception.code,'PROMPT_UNAVAILABLE')
        self.service.config = self.config
        with self.assertRaises(ApiError): self.run_call(body={**self.body,'images':['http://127.0.0.1/secret']})
        self.assertEqual(self.calls, [])

    def test_existing_glm_alias_is_available_without_changing_default(self):
        self.service.config = replace(self.config, assistant_model='another-default', assistant_models=({'id':'glm', 'label':'GLM', 'model':MODEL},))
        self.assertTrue(self.api.capabilities()['rewrite'])
        self.service.config = replace(self.service.config, assistant_base_url='https://different.example')
        self.assertFalse(self.api.capabilities()['rewrite'])

if __name__ == '__main__': unittest.main()
