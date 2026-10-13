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
    "retry_canvas_registration", "flow_read", "flow_write",
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


class ManagedEditorToolsTest(AssistantFixture):
    def test_flow_write_reaches_provider_and_settles_before_delivery(self):
        body = self.body()
        body["request"]["tools"][0]["name"] = "flow_write"
        event = done(tool=True)
        event["message"]["toolCalls"][0]["name"] = "flow_write"
        self.provider.events = [event]
        operation = self.create(body)
        self.assertEqual(self.worker.process_once(), 1)
        self.assertEqual(self.service.get(self.user, operation["operation_id"])["state"], "succeeded")
        events = self.service.events(self.user, operation["operation_id"], 0)["events"]
        output = next(e for e in events if e["type"] == "model" and e["payload"]["type"] == "done")
        self.assertEqual(output["payload"]["message"]["toolCalls"][0]["name"], "flow_write")
        self.assertLess(next(i for i,e in enumerate(events) if e["type"] == "billing.changed"), events.index(output))

    def test_provider_cannot_introduce_unrequested_editor_tool(self):
        event = done(tool=True)
        event["message"]["toolCalls"][0]["name"] = "flow_write"
        self.provider.events = [event]
        operation = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, operation["operation_id"])["state"], "outcome_unknown")
        events = self.service.events(self.user, operation["operation_id"], 0)["events"]
        self.assertFalse(any(e["type"] == "model" and e["payload"].get("message", {}).get("toolCalls") for e in events))
