"""Editor tool vocabulary survives both managed request and response validation."""
import copy
import unittest
from types import SimpleNamespace

from seecut_server.assistant_contract import TOOLS, RUNTIME_TOOLS, model_request, model_event
from seecut_server.errors import ApiError
from test_assistant_contract import AssistantFixture, done

EDITOR_TOOLS = (
    "app_context", "page_items", "page_preview", "app_navigate", "app_operation",
    "capabilities", "context", "project", "preview", "clip_preview", "move_selected_image",
    "library_images", "library_assets", "insert_library_image", "transform_canvas_layer", "canvas_edit",
    "save_canvas_project", "export_canvas_png", "canvas_operation",
    "retry_canvas_registration", "clip_edit", "clip_operation", "clip_export_control", "clip_export_output",
    "flow_get_workspace", "flow_read_canvas", "flow_list_node_types", "flow_get_node_tools", "flow_get_node_config", "flow_get_task", "flow_get_operation",
    "flow_add_node", "flow_update_node", "flow_connect_nodes", "flow_delete_nodes", "flow_switch_canvas", "flow_execute_node",
)


class EditorVocabularyTest(unittest.TestCase):
    def test_complete_catalog_and_tool_history_round_trip(self):
        self.assertEqual(set(EDITOR_TOOLS), TOOLS)
        request = {"system": "test", "messages": [], "maxOutputTokens": 100,
                   "tools": [{"name": name, "description": "editor", "parameters": {"type": "object"}}
                             for name in EDITOR_TOOLS]}
        for index, name in enumerate(EDITOR_TOOLS):
            reply = {"id": f"reply-{index}", "role": "assistant", "content": [],
                     "toolCalls": [{"id": f"call-{index}", "name": name, "arguments": {}}]}
            model_event({"type": "done", "message": reply, "stopReason": "tools"})
            request["messages"].extend([reply, {"id": f"result-{index}", "role": "tool",
                "toolCallId": f"call-{index}", "toolName": name,
                "content": [{"type": "text", "text": "{}"}]}])
        model_request(request, SimpleNamespace(assistant_max_output_tokens=100, assistant_images=False))
        for bad in ("shell", "flow_delete", "filesystem"):
            altered = copy.deepcopy(request)
            altered["tools"][0]["name"] = bad
            with self.assertRaises(ApiError):
                model_request(altered, SimpleNamespace(assistant_max_output_tokens=100, assistant_images=False))

    def test_retired_flow_history_is_readable_without_restoring_tools(self):
        config = SimpleNamespace(assistant_max_output_tokens=100, assistant_images=False)
        for name in ("flow_read", "flow_write"):
            with self.subTest(tool=name):
                reply = {"id": "old-reply", "role": "assistant", "content": [],
                         "toolCalls": [{"id": "old-call", "name": name, "arguments": {}}]}
                result = {"id": "old-result", "role": "tool", "toolCallId": "old-call",
                          "toolName": name, "content": [{"type": "text", "text": "{}"}]}
                request = {"system": "test", "messages": [reply, result], "maxOutputTokens": 100,
                           "tools": [{"name": "context", "description": "editor", "parameters": {"type": "object"}}]}
                self.assertEqual(model_request(request, config), request)
                with self.assertRaises(ApiError):
                    model_event({"type": "done", "message": reply, "stopReason": "tools"})
                request["tools"][0]["name"] = name
                with self.assertRaises(ApiError):
                    model_request(request, config)


class ManagedEditorToolsTest(AssistantFixture):
    def test_runtime_tools_are_announced_once_outside_model_policy(self):
        catalog = self.service.capabilities()
        self.assertEqual(catalog['runtime_tools'], {'version': 1, 'names': ['task_checkpoint', 'task_status']})
        self.assertTrue(RUNTIME_TOOLS.isdisjoint(TOOLS))
        self.assertTrue(all('runtime_tools' not in model for model in catalog['models']))
        self.assertNotIn('runtime_tools', self.service.capabilities(include_models=False))

    def test_runtime_tool_request_response_and_history_round_trip(self):
        config = SimpleNamespace(assistant_max_output_tokens=100, assistant_images=False)
        for name in sorted(RUNTIME_TOOLS):
            with self.subTest(tool=name):
                reply = {'id': 'reply', 'role': 'assistant', 'content': [],
                         'toolCalls': [{'id': 'call-1', 'name': name, 'arguments': {}}]}
                model_event({'type': 'done', 'message': reply, 'stopReason': 'tools'})
                request = {'system': 'test', 'messages': [reply, {'id': 'result', 'role': 'tool',
                    'toolCallId': 'call-1', 'toolName': name, 'content': [{'type': 'text', 'text': '{}'}]}],
                    'tools': [{'name': name, 'description': 'runtime', 'parameters': {'type': 'object'}}],
                    'maxOutputTokens': 100}
                self.assertEqual(model_request(request, config), request)

    def test_unrequested_runtime_tool_is_not_delivered(self):
        for name in sorted(RUNTIME_TOOLS):
            with self.subTest(tool=name):
                self.user, _ = self.new_user(name + '@example.test')
                event = done(tool=True)
                event['message']['toolCalls'][0]['name'] = name
                self.provider.events = [event]
                operation = self.create(self.body(step='step-' + name, turn='turn-' + name))
                self.worker.process_once()
                self.assertEqual(self.service.get(self.user, operation['operation_id'])['state'], 'outcome_unknown')
                events = self.service.events(self.user, operation['operation_id'], 0)['events']
                self.assertFalse(any(e['type'] == 'model' and e['payload'].get('message', {}).get('toolCalls') for e in events))

    def test_retired_flow_tools_are_rejected_before_provider_submission(self):
        for name in ("flow_read", "flow_write"):
            with self.subTest(tool=name):
                body = self.body()
                body["request"]["tools"][0]["name"] = name
                with self.assertRaises(ApiError) as error:
                    self.create(body)
                self.assertEqual(error.exception.code, "ASSISTANT_INVALID_REQUEST")
        self.assertEqual(self.worker.process_once(), 0)

    def test_provider_cannot_introduce_unrequested_asset_or_clip_tool(self):
        for name in ("library_assets", "clip_preview", "clip_export_control", "clip_export_output"):
            with self.subTest(tool=name):
                self.user, _ = self.new_user(name + "@unrequested-editor.example.test")
                event = done(tool=True)
                event["message"]["toolCalls"][0]["name"] = name
                self.provider.events = [event]
                operation = self.create(self.body(step="unrequested-" + name, turn="turn-" + name))
                self.worker.process_once()
                self.assertEqual(self.service.get(self.user, operation["operation_id"])["state"], "outcome_unknown")
                events = self.service.events(self.user, operation["operation_id"], 0)["events"]
                self.assertFalse(any(e["type"] == "model" and e["payload"].get("message", {}).get("toolCalls") for e in events))

    def test_provider_cannot_introduce_unrequested_app_read_tools(self):
        for name in ("app_context", "page_items", "page_preview"):
            with self.subTest(tool=name):
                self.user, _ = self.new_user(name + "@example.test")
                event = done(tool=True)
                event["message"]["toolCalls"][0]["name"] = name
                self.provider.events = [event]
                operation = self.create(self.body(step="step-" + name, turn="turn-" + name))
                self.worker.process_once()
                self.assertEqual(self.service.get(self.user, operation["operation_id"])["state"], "outcome_unknown")
                events = self.service.events(self.user, operation["operation_id"], 0)["events"]
                self.assertFalse(any(e["type"] == "model" and e["payload"].get("message", {}).get("toolCalls") for e in events))

    def test_provider_cannot_introduce_unrequested_editor_tool(self):
        event = done(tool=True)
        event["message"]["toolCalls"][0]["name"] = "flow_write"
        self.provider.events = [event]
        operation = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, operation["operation_id"])["state"], "outcome_unknown")
        events = self.service.events(self.user, operation["operation_id"], 0)["events"]
        self.assertFalse(any(e["type"] == "model" and e["payload"].get("message", {}).get("toolCalls") for e in events))


class AppControlVocabularyTest(AssistantFixture):
    def test_requested_navigation_and_query_keep_original_model_calls_and_history(self):
        for name in ("app_navigate", "app_operation"):
            with self.subTest(tool=name):
                self.user, _ = self.new_user(name + "@requested-control.example.test")
                body = self.body(step="control-" + name, turn="turn-" + name)
                body["request"]["tools"] = [{"name": name, "description": "App transport only",
                    "parameters": {"type": "object", "properties": {"target": {"type": "string"}}}}]
                event = done(tool=True)
                call = event["message"]["toolCalls"][0]
                call["name"] = name
                call["arguments"] = {"target": "personalLibrary"} if name == "app_navigate" else {"operationId": "sca1:1:6:client:4:chat:1"}
                self.provider.events = [event]
                operation = self.create(body)
                self.worker.process_once()
                self.assertEqual(self.service.get(self.user, operation["operation_id"])["state"], "succeeded")
                events = self.service.events(self.user, operation["operation_id"], 0)["events"]
                delivered = [e["payload"] for e in events if e["type"] == "model" and e["payload"].get("type") == "done"]
                self.assertEqual(delivered[0]["message"]["toolCalls"][0], call)
                # Only vocabulary changes: model transport neither grants native
                # control nor accepts a new business action protocol.
                request = copy.deepcopy(body["request"])
                request["messages"] += [event["message"], {"id": "result", "role": "tool", "toolCallId": call["id"], "toolName": name, "content": [{"type": "text", "text": "{}"}]}]
                self.assertEqual(model_request(request, self.config), request)

    def test_unrequested_control_words_are_not_delivered_by_provider(self):
        for name in ("app_navigate", "app_operation"):
            with self.subTest(tool=name):
                self.user, _ = self.new_user(name + "@control.example.test")
                event = done(tool=True)
                event["message"]["toolCalls"][0]["name"] = name
                self.provider.events = [event]
                operation = self.create(self.body(step="unrequested-" + name, turn="turn-" + name))
                self.worker.process_once()
                self.assertEqual(self.service.get(self.user, operation["operation_id"])["state"], "outcome_unknown")
                events = self.service.events(self.user, operation["operation_id"], 0)["events"]
                self.assertFalse(any(e["type"] == "model" and e["payload"].get("message", {}).get("toolCalls") for e in events))

    def test_only_two_new_words_not_open_project_actions_or_filesystem(self):
        for name in ("app_open_project", "navigate-resume", "app_action", "filesystem", "shell"):
            with self.subTest(tool=name):
                self.user, _ = self.new_user(name.replace("_", "-") + "@bad-control.example.test")
                body = self.body(step="bad-" + name, turn="turn-" + name)
                body["request"]["tools"][0]["name"] = name
                with self.assertRaises(ApiError):
                    model_request(body["request"], self.config)
                event = done(tool=True)
                event["message"]["toolCalls"][0]["name"] = name
                with self.assertRaises(ApiError):
                    model_event(event)
