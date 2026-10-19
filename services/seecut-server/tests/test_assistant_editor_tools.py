"""Editor tool vocabulary survives both managed request and response validation."""
import copy
import unittest
from types import SimpleNamespace

from seecut_server.assistant_contract import model_request, model_event
from seecut_server.errors import ApiError
from test_assistant_contract import AssistantFixture, done

EDITOR_TOOLS = (
    "capabilities", "context", "project", "preview", "move_selected_image",
    "library_images", "insert_library_image", "transform_canvas_layer",
    "save_canvas_project", "export_canvas_png", "canvas_operation",
    "retry_canvas_registration",
    "flow_get_workspace", "flow_read_canvas", "flow_list_node_types", "flow_get_node_tools", "flow_get_node_config", "flow_get_task", "flow_get_operation",
    "flow_add_node", "flow_update_node", "flow_connect_nodes", "flow_delete_nodes", "flow_switch_canvas", "flow_execute_node",
)


class EditorVocabularyTest(unittest.TestCase):
    def test_complete_catalog_and_tool_history_round_trip(self):
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
    def test_retired_flow_tools_are_rejected_before_provider_submission(self):
        for name in ("flow_read", "flow_write"):
            with self.subTest(tool=name):
                body = self.body()
                body["request"]["tools"][0]["name"] = name
                with self.assertRaises(ApiError) as error:
                    self.create(body)
                self.assertEqual(error.exception.code, "ASSISTANT_INVALID_REQUEST")
        self.assertEqual(self.worker.process_once(), 0)

    def test_provider_cannot_introduce_unrequested_editor_tool(self):
        event = done(tool=True)
        event["message"]["toolCalls"][0]["name"] = "flow_write"
        self.provider.events = [event]
        operation = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, operation["operation_id"])["state"], "outcome_unknown")
        events = self.service.events(self.user, operation["operation_id"], 0)["events"]
        self.assertFalse(any(e["type"] == "model" and e["payload"].get("message", {}).get("toolCalls") for e in events))
