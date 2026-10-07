#!/usr/bin/env python3
"""Run one normal Linux App and an independent, trusted black-box QA script.

Legacy QA receives --app-pid, --client-binary, --work-dir, --output, --expected-sha,
--deadline-monotonic, and --next-stage. An absent optional client is passed as a nonexistent
absolute work path. QA records /proc binary identity and the unique visible X11
window ID itself. It must finish before the monotonic deadline, leave regular
files in output or its known observation/navigation directories, and never
include secrets in artifacts. Optional --identity-approval and --ui-approval are
copied verbatim and forwarded; fixed reviewed stages verify their raw identities.
An optional candidate manifest binds the copied executable to its actual source
HEAD, build run/job and content hash before launch. Explicit source HEAD and
artifact ID allow a reviewed test-only workflow revision to use the original App.
The explicit workflow-observation stage instead uses its reviewed fixed script
and helper, window ID, approvals and isolated-display flags without legacy args.
The fresh-workbench-observation stage uses its fixed script and metadata helper,
exact candidate, window ID, approvals and private accessibility bus, with no client
or fixture inputs. Settings entry permanently ends pixel capture.
The two A2 observation stages each run one fixed, independently reviewed
controller scope on a fresh App, with finite scope artifacts and a 120s controller.
QA receives the actual App source HEAD. This contract supports one App lifetime:
QA must not launch another App,
detach descendants, or move them into new process groups. This harness supplies
no UI navigation, authorization, product assertions, or product test results.
It is resource isolation for trusted QA, not a security sandbox: HOME and X11
remain available. A zero harness exit establishes only QA exit and cleanup.
"""

import argparse
from collections import deque
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time


LOG_LIMIT = 256 * 1024
ARTIFACT_LIMIT = 15 * 1024 * 1024
LINE_LIMIT = 16 * 1024
SENSITIVE = re.compile(rb"token|authorization|credential|password|secret|bearer", re.I)
RESERVED = {"harness.json", "app.log", "qa.log"}
QA_DIRECTORIES = {"independent-qa-observation", "independent-qa-navigation"}
ALPHA_STAGE = 'alpha-workflow-observation'
ALPHA_HEAD = '0aa9406247e53f073c0b4df686adc68b40e2f8f6'
ALPHA_APP_SHA = '03fb1752c34adb0c0f4a21521c2302ee80c9b23203728cbcf636d3a4a57eaea2'
ALPHA_BUILD_RUN = 37385930165
ALPHA_BUILD_JOB = 112019086814
ALPHA_ARTIFACT = '11380395799'
ALPHA_APP_BYTES = 467641344
ALPHA_MANIFEST_SHA = 'd666628e2499a26ef5f541898a4b5f0c8a69bba98bc6a3a28a2725d37c426d73'
ALPHA_DIRECTORY = 'main-qa-alpha-workflow'
ALPHA_SCRIPTS = {'qa': ('alpha-workflow-controller.py', 'c72370a11ba53582bb4963b6275ec4837f296b5930bcb4aa689845544ef341ad'), 'public_ui_probe': ('alpha_public_probe.py', 'adcb9d18a3020f2fd4e7661d8822680b1fc1fdf76f5878ea7f5ef3849b1e1a66'), 'workflow_checks': ('alpha_checks.py', '7ca9c3e29cefa231ea6dfd2579fde5e2d60abc9d17a74681f3960d25e3894c32'), 'native_ui_action': ('alpha_native_action.py', '99eed0c32e42edfcad9bce12fde6b1dd36b2247a09479378f3429c8b24fea098')}
ALPHA_UI_SHA = 'd35a0d904166e8f2fefcea0ba40a83d31eb6f56cdbe5326d9fac604f4efb4792'
ALPHA_IDENTITY_SHA = '0eb6fcacf68a3c1028eafa1f29f0e1435c7347103509a82421dd0f43d3df7d41'
ALPHA_PNG_NAMES = frozenset(['01-current-initial.png', '02-after-quick.png', '03-gallery.png', '04-create-dialog.png', '05-editor.png', '06-open-menu.png', '07-import-ambiguous-native.png', '07-import-native-outside-display.png', '07-import-native-visible.png', '07-import-no-visible-native-after-wait.png', '08-import-fixture-not-confirmed-after-wait.png', '08-import-fixture-visible-stable.png', '08-import-location-visible.png', '08-import-menu-closed.png', '08-import-native-chrome.png', '08-import-native-not-gone.png', '08-import-returned-App.png', '09-edit-dragged.png', '09-edit-one-Undo.png', '10-save-ambiguous-native.png', '10-save-native-outside-display.png', '10-save-native-visible.png', '10-save-no-visible-native-after-wait.png', '11-save-returned-App.png', '12-save-gallery.png', '13-gallery-other-blank.png', '14-gallery-after-other-document.png', '15-gallery-reopen-fixture-not-confirmed-after-wait.png', '15-gallery-reopen-fixture-visible-stable.png', '19-export-destination.png', '20-export-App-destination-requires-review.png', '20-export-ambiguous-native.png', '20-export-native-outside-display.png', '20-export-native-visible.png', '20-export-no-visible-native-after-wait.png', '21-export-ambiguous-native.png', '21-export-location-visible.png', '21-export-menu-closed.png', '21-export-native-chrome.png', '21-export-native-not-gone.png', '21-export-native-outside-display.png', '21-export-native-visible.png', '21-export-no-visible-native-after-wait.png', '21-export-returned-App.png', '21-export-unreviewed-destination.png', 'exported-qa.png'])
ALPHA_JSON_NAMES = frozenset(['08-import-public-before-accept.json', '08-import-public-before.json', '08-import-public-location.json', '20-export-public.json', '21-export-public-before-accept.json', '21-export-public-before.json', '21-export-public-location.json', 'capture-01-current-initial.json', 'capture-02-after-quick.json', 'capture-03-gallery.json', 'capture-04-create-dialog.json', 'capture-05-editor.json', 'capture-06-open-menu.json', 'capture-07-import-ambiguous-native.json', 'capture-07-import-native-outside-display.json', 'capture-07-import-native-visible.json', 'capture-07-import-no-visible-native-after-wait.json', 'capture-08-import-fixture-not-confirmed-after-wait.json', 'capture-08-import-fixture-visible-stable.json', 'capture-08-import-location-visible.json', 'capture-08-import-menu-closed.json', 'capture-08-import-native-chrome.json', 'capture-08-import-native-not-gone.json', 'capture-08-import-returned-app.json', 'capture-09-edit-dragged.json', 'capture-09-edit-one-undo.json', 'capture-10-save-ambiguous-native.json', 'capture-10-save-native-outside-display.json', 'capture-10-save-native-visible.json', 'capture-10-save-no-visible-native-after-wait.json', 'capture-11-save-returned-app.json', 'capture-12-save-gallery.json', 'capture-13-gallery-other-blank.json', 'capture-14-gallery-after-other-document.json', 'capture-15-gallery-reopen-fixture-not-confirmed-after-wait.json', 'capture-15-gallery-reopen-fixture-visible-stable.json', 'capture-19-export-destination.json', 'capture-20-export-ambiguous-native.json', 'capture-20-export-app-destination-requires-review.json', 'capture-20-export-native-outside-display.json', 'capture-20-export-native-visible.json', 'capture-20-export-no-visible-native-after-wait.json', 'capture-21-export-ambiguous-native.json', 'capture-21-export-location-visible.json', 'capture-21-export-menu-closed.json', 'capture-21-export-native-chrome.json', 'capture-21-export-native-not-gone.json', 'capture-21-export-native-outside-display.json', 'capture-21-export-native-visible.json', 'capture-21-export-no-visible-native-after-wait.json', 'capture-21-export-returned-app.json', 'capture-21-export-unreviewed-destination.json', 'gallery-public-blank.json', 'gallery-public-image.json', 'native-proof-01-current-initial.json', 'native-proof-02-after-quick.json', 'native-proof-03-gallery.json', 'native-proof-04-create-dialog.json', 'native-proof-05-editor.json', 'native-proof-06-open-menu.json', 'native-proof-07-import-ambiguous-native.json', 'native-proof-07-import-native-outside-display.json', 'native-proof-07-import-native-visible.json', 'native-proof-07-import-no-visible-native-after-wait.json', 'native-proof-08-import-fixture-not-confirmed-after-wait.json', 'native-proof-08-import-fixture-visible-stable.json', 'native-proof-08-import-location-visible.json', 'native-proof-08-import-menu-closed.json', 'native-proof-08-import-native-chrome.json', 'native-proof-08-import-native-not-gone.json', 'native-proof-08-import-returned-app.json', 'native-proof-09-edit-dragged.json', 'native-proof-09-edit-one-undo.json', 'native-proof-10-save-ambiguous-native.json', 'native-proof-10-save-native-outside-display.json', 'native-proof-10-save-native-visible.json', 'native-proof-10-save-no-visible-native-after-wait.json', 'native-proof-11-save-returned-app.json', 'native-proof-12-save-gallery.json', 'native-proof-13-gallery-other-blank.json', 'native-proof-14-gallery-after-other-document.json', 'native-proof-15-gallery-reopen-fixture-not-confirmed-after-wait.json', 'native-proof-15-gallery-reopen-fixture-visible-stable.json', 'native-proof-19-export-destination.json', 'native-proof-20-export-ambiguous-native.json', 'native-proof-20-export-app-destination-requires-review.json', 'native-proof-20-export-native-outside-display.json', 'native-proof-20-export-native-visible.json', 'native-proof-20-export-no-visible-native-after-wait.json', 'native-proof-21-export-ambiguous-native.json', 'native-proof-21-export-location-visible.json', 'native-proof-21-export-menu-closed.json', 'native-proof-21-export-native-chrome.json', 'native-proof-21-export-native-not-gone.json', 'native-proof-21-export-native-outside-display.json', 'native-proof-21-export-native-visible.json', 'native-proof-21-export-no-visible-native-after-wait.json', 'native-proof-21-export-returned-app.json', 'native-proof-21-export-unreviewed-destination.json', 'saved-title-public-0.json', 'saved-title-public-1.json', 'saved-title-public-2.json', 'workflow.json'])

WORKFLOW_DIRECTORY = "independent-qa-workflow"
WORKFLOW_HEAD = 'a6cf09ad935bbbc6cf792c4ac6a4b506a47aebd3'
WORKFLOW_APP_SHA = 'b7ec5fc769d09e3685f212cd96484dfbb0858e9edb649d1314877a5f0a86f852'
WORKFLOW_BUILD_RUN = 37340470010
WORKFLOW_ARTIFACT = '11359607955'
WORKFLOW_UI = ('style-workflow-ui.json', '0259955a83ece9bd0f1d8cac1bd184900c0db27e8837914d69131c049f9cad97')
WORKFLOW_IDENTITY_SHA = 'bcf5c889a038166d7669d229c04cbf084ca7154ab3d83244042279f805fba7d3'
WORKFLOW_SCRIPTS = {
    'qa': ('style-workflow-controller.py', '9a3e965fcf0d619efa62d6495773880333e1290454b94c02291801cf406a2d19'),
    'public_ui_probe': ('public_ui_probe.py', 'd6e74b62eaaca096ec75f55fd0326942dbece24dfb0e32397062c1248a3c23e2'),
    'workflow_checks': ('workflow_checks.py', '93b6246ec2f0bb450753812bc0eacc0493480ab1bee9acccf88cd19210bfe853'),
    'native_ui_action': ('style-native-ui-action.py', 'a89dcf2431a5a38a5b94fa9ffc537785ef54849623e66b6ae981c97ff441a45f'),
}
WORKFLOW_PNG_NAMES = {
    name + ".png" for name in (
        "01-current-initial", "02-after-quick", "03-gallery", "04-create-dialog", "05-editor", "06-open-menu",
        "12-left-saved-editor", "13-new-blank-dialog", "14-new-blank-editor", "15-reopen-menu", "19-export-destination",
        "12-save-gallery", "13-gallery-other-blank", "14-gallery-after-other-document",
        "17-secondary-project-control-hover", "20-export-App-destination-requires-review",
        "21-export-native-visible", "21-export-ambiguous-native", "21-export-native-outside-display",
        "21-export-no-visible-native-after-wait", "21-export-unreviewed-destination",
    )
} | {
    phase + suffix + ".png"
    for phase in ("07-import", "10-save", "16-reopen", "20-export")
    for suffix in ("-ambiguous-native", "-native-outside-display", "-native-visible", "-no-visible-native-after-wait")
} | {
    phase + suffix + ".png"
    for phase in ("08-import", "11-save", "17-reopen", "21-export")
    for suffix in ("-returned-App", "-menu-closed", "-native-not-gone", "-native-chrome", "-location-visible")
} | {
    phase + suffix + ".png"
    for phase in ("09-edit", "16-gallery-reopened-edit", "18-reopened-edit") for suffix in ("-dragged", "-one-Undo")
} | {
    phase + suffix + ".png"
    for phase in ("08-import", "15-gallery-reopen", "17-reopen")
    for suffix in ("-fixture-visible-stable", "-fixture-not-confirmed-after-wait")
}
WORKFLOW_JSON_NAMES = {"workflow.json", "public-accessibility.json", "12-save-gallery-public.json"} | {
    phase + suffix + ".json"
    for phase in ("08-import", "11-save", "17-reopen", "21-export")
    for suffix in ("-public-before", "-public-location", "-public-before-accept")
} | {"20-export-public.json"}
WORKFLOW_FILE_LIMIT = 2 * 1024 * 1024
WORKFLOW_PNG_LIMIT = 14 * 1024 * 1024
WORKFLOW_METADATA_LIMIT = 128 * 1024
NEXT_UI_SCRIPT = ("independent-next-ui.py", "0f6bea99d3ff0a8d9c884a697e416409b1dc89692ccf78d3dc118af9ca7af476")
NEXT_UI_PROBE = ("public_probe_cecc8fd_ui2.py", "8db69027991a7fe49beb8f4515926b7fd932436c0034ccffd03d6a543f36954b")
NEXT_UI_HEAD = "cecc8fdf9578675051dae58bda25f0ff805ce235"
NEXT_UI_APP_SHA = "8859b10e7b8a58785d6a60454995f33d56efea554011825287917d06064aff55"
NEXT_UI_BUILD_RUN = 37312315509
NEXT_UI_ARTIFACT = "11345919609"
NEXT_UI_DIRECTORY = "independent-qa-cecc8fd-ui2"
NEXT_UI_PNG_NAMES = {
    "01-before-quick.png", "02-after-quick-1280x900.png",
    "03-workbench-Tab.png", "04-workbench-ShiftTab.png",
    "05-after-canvas-navigation.png", "06-after-clip-navigation.png", "07-after-assets-navigation.png",
}
NEXT_UI_JSON_NAMES = {
    "navigation.json", "03-workbench-tab-public.json", "04-workbench-shifttab-public.json",
    "08-settings-public.json", "09-settings-tab-public.json", "10-settings-shifttab-public.json",
    "11-settings-after-escape-public.json",
}
A2_SCOPES = {
    "settings-controls-observation": "settings-controls",
    "canvas-entry-observation": "canvas-entry",
}
# Final independently delivered byte identities; controller/helpers are not adapters.
A2_HEAD = "11ebf203e1a78d3b6a21677c4b17e96223c74b0e"
A2_APP_SHA = "8fe30fc73f4ab79435e79aa582edf4e2b3adeb26a5435315ea32b537c28abf30"
A2_BUILD_RUN = 37328783317
A2_ARTIFACT = "11355501142"
A2_SCRIPTS = {
    "qa": ("next_ui_11ebf20_a3.py", "6a9b01710b0fda386a133cf57c93a650d183378ee366621327fcc1013c464f4f"),
    "a2_probe": ("public_probe_11ebf20_ui4.py", "ef194ed6b55e545c922308f875aed184d76490530c8f2a88a27459ce3f1994bd"),
    "a2_action": ("public_action_11ebf20_ui4.py", "8aafd63bc2653c1099003edbf5808c1c4550471d31f619cf1851a0e7ea063518"),
}
A2_UI = ("reviewed-ui-11ebf20-a3.json", "3770f759f4479904651f3e3404e0ff23b6bea08c3a007320bf2d915ff7c93852")
A2_IDENTITY_SHA = "2b94fc606a46d7d0df43ea2d07a29e5a3b5075253521a201e65d264ee5933574"
A2_CONTROLLER_SECONDS = 120
A2_DIRECTORIES = {stage: "main-qa-11ebf20-a3-" + scope for stage, scope in A2_SCOPES.items()}
A2_PNG_NAMES = {
    "settings-controls": {
        "01-before-quick.png", "02-after-quick-1280x900.png", "07-after-close-workbench-1280x900.png",
        "08-after-close-workbench-1024x900.png", "08-after-close-workbench-1440x900.png",
    },
    "canvas-entry": {
        "01-before-quick.png", "02-after-quick-1280x900.png", "03-canvas-gallery-1280x900.png",
        "04-canvas-gallery-1024x900.png", "04-canvas-gallery-1440x900.png", "05-canvas-gallery-return-1280x900.png",
        "06-current-new-canvas-dialog.png", "07-current-create-result.png",
        "08-current-blank-editor-1024x900.png", "08-current-blank-editor-1440x900.png",
    },
}
A2_PUBLIC_JSON_NAMES = {
    "settings-controls": {"03-settings-expanded-public.json", "04-professional-result-public.json",
                          "05-dark-result-public.json", "06-after-close-public.json"},
    "canvas-entry": {"06-current-new-canvas-dialog-public.json", "07-create-result-public.json"},
}
A2_ACTION_JSON_NAMES = {
    "settings-controls": {"04-professional-action.json", "05-dark-action.json", "06-close-settings-action.json"},
    "canvas-entry": set(),
}
A2_ACTION_LIMIT = 8 * 1024

# Only these additional native scopes use the new finite spec table.
NATIVE_HEAD = '11ebf203e1a78d3b6a21677c4b17e96223c74b0e'
NATIVE_APP_SHA = '8fe30fc73f4ab79435e79aa582edf4e2b3adeb26a5435315ea32b537c28abf30'
NATIVE_BUILD_RUN = 37328783317
NATIVE_ARTIFACT = '11355501142'
NATIVE_IDENTITY_SHA = '2b94fc606a46d7d0df43ea2d07a29e5a3b5075253521a201e65d264ee5933574'
# These scopes observe the new App against unchanged source11 entry guards.
CLIP_EDITOR_RUNTIME = ('0aa9406247e53f073c0b4df686adc68b40e2f8f6', '03fb1752c34adb0c0f4a21521c2302ee80c9b23203728cbcf636d3a4a57eaea2', 37385930165, '11380395799', '9248168645ce9324be4cfab71e8e9d851c14e0a784050a6fc84dad68599d0a7e')


def native_runtime_identity(stage):
    if stage == 'asset-library-flow':
        return (*CLIP_EDITOR_RUNTIME[:4], '93d8545ed799bda4abf55c7612f5c94904749f36011cf993efa8668f5fd99ae8')
    if stage == 'assistant-config-observation':
        return (*CLIP_EDITOR_RUNTIME[:4], '5a1d5b1630db3bc097a8ed0f201938d53371270af0599879ef76bb98208d969e')
    if stage in ('clip-editor-entry', 'clip-media-import'):
        return CLIP_EDITOR_RUNTIME
    return (NATIVE_HEAD, NATIVE_APP_SHA, NATIVE_BUILD_RUN, NATIVE_ARTIFACT, NATIVE_IDENTITY_SHA)


NATIVE_CONTROLLER_SECONDS = 120
NATIVE_SCOPES = {'workspaces-observation': {'scripts': {'qa': ['workspaces-ui-11ebf20.py',
                                               '3335bd6160a5fa38195430861e1065cd6e4b0d8b261747c455c6d9c2409b2b0a'],
                                        'native_public_probe': ['public_probe_11ebf20_ui4.py',
                                                                'ef194ed6b55e545c922308f875aed184d76490530c8f2a88a27459ce3f1994bd']},
                            'ui': ['workspaces-reviewed-ui-11ebf20.json',
                                   'b50fd9ca0b7624473793b554b3bbae9cda94fc61b0757156b72a8eb8619716fe'],
                            'next_stage': None,
                            'artifacts': {'main-qa-11ebf20-workspaces': {'png': ['01-after-quick-1280x900.png',
                                                                                 '02-clip-1280x900.png',
                                                                                 '03-clip-1024x900.png',
                                                                                 '04-clip-1440x900.png',
                                                                                 '05-clip-1280x720.png',
                                                                                 '06-assets-1280x900.png',
                                                                                 '07-assets-1024x900.png',
                                                                                 '08-assets-1440x900.png',
                                                                                 '09-assets-1280x720.png',
                                                                                 '10-return-generation-1280x900.png'],
                                                                         'public': ['workspaces.json',
                                                                                    '01-after-quick-1280x900-public.json',
                                                                                    '02-clip-1280x900-public.json',
                                                                                    '03-clip-1024x900-public.json',
                                                                                    '04-clip-1440x900-public.json',
                                                                                    '05-clip-1280x720-public.json',
                                                                                    '06-assets-1280x900-public.json',
                                                                                    '07-assets-1024x900-public.json',
                                                                                    '08-assets-1440x900-public.json',
                                                                                    '09-assets-1280x720-public.json',
                                                                                    '10-return-generation-1280x900-public.json',
                                                                                    '01-after-quick-prepixels-public.json',
                                                                                    '02-before-clip-public.json',
                                                                                    '06-before-assets-public.json',
                                                                                    '10-before-generate-public.json',
                                                                                    '10-return-generation-prepixels-public.json'],
                                                                         'action': []}}},
 'assistant-entry-observation': {'scripts': {'qa': ['agent-native-entry-controller.py',
                                                    '739940c925145f49136d7cc618a01f3b7775b4ae84cbb1d2647221032270f3e4'],
                                             'native_agent_native_controller': ['agent-native-entry-controller.py',
                                                                               '739940c925145f49136d7cc618a01f3b7775b4ae84cbb1d2647221032270f3e4'],
                                             'native_agent_native_bootstrap': ['agent-native-bootstrap.py',
                                                                               '4dfd9d275a5639e85a3ebce71fbaa620e1e3ca3f9982199887d04a65f6933439'],
                                             'native_agent_native_entry_action': ['agent-native-entry-action.py',
                                                                                  '3545a09ce95f583c6e738c3e18040cc0a671662304c366807d617af51f437617'],
                                             'native_agent_native_public_probe': ['agent-native-public-probe.py',
                                                                                  'c0d9f9e0fe477e9e1592c765c49d197aaf841da0e4ca92114f63f32d417ecfa2'],
                                             'native_public_probe_11ebf20_ui4': ['public_probe_11ebf20_ui4.py',
                                                                                 'ef194ed6b55e545c922308f875aed184d76490530c8f2a88a27459ce3f1994bd'],
                                             'native_public_action_11ebf20_ui4': ['public_action_11ebf20_ui4.py',
                                                                                  '8aafd63bc2653c1099003edbf5808c1c4550471d31f619cf1851a0e7ea063518']},
                                 'ui': ['reviewed-ui-11ebf20-a3.json',
                                        '3770f759f4479904651f3e3404e0ff23b6bea08c3a007320bf2d915ff7c93852'],
                                 'next_stage': 'canvas-entry',
                                 'artifacts': {'main-qa-agent-native-bootstrap': {'png': ['01-before-quick.png',
                                                                                          '02-after-quick-1280x900.png',
                                                                                          '03-canvas-gallery-1280x900.png',
                                                                                          '06-current-new-canvas-dialog.png',
                                                                                          '07-current-create-result.png'],
                                                                                  'public': ['06-current-new-canvas-dialog-public.json',
                                                                                             '07-create-result-public.json',
                                                                                             'bootstrap.json'],
                                                                                  'action': []},
                                               'main-qa-agent-native-entry': {'png': ['01-before-hover.png',
                                                                                      '03-assistant-tooltip-review.png',
                                                                                      '04-assistant-panel-1280x900.png',
                                                                                      '05-assistant-panel-1024x900.png',
                                                                                      '06-assistant-panel-1440x900.png',
                                                                                      '07-assistant-panel-1280x720.png'],
                                                                              'public': ['01-before-hover-public.json',
                                                                                         '02-recheck-public.json',
                                                                                         '03-after-hover-public.json',
                                                                                         '03-before-open-public.json',
                                                                                         '04-panel-public.json',
                                                                                         '05-panel-1024x900-public.json',
                                                                                         '06-panel-1440x900-public.json',
                                                                                         '07-panel-1280x720-public.json',
                                                                                         'agent-native-entry.json'],
                                                                              'action': ['04-entry-action.json']}}}}

NATIVE_SCOPES.update({'assistant-config-observation': {'scripts': {'qa': ['agent-config-controller.py',
                                                     '9c431bf1b9fc788ac3f57e46df28d4570a2e1592e047f7fa8422ac9a12d84178'],
                                              'native_agent_config_controller': ['agent-config-controller.py',
                                                                                 '9c431bf1b9fc788ac3f57e46df28d4570a2e1592e047f7fa8422ac9a12d84178'],
                                              'native_agent_config_action': ['agent-config-action.py',
                                                                             '125c29c64416311a7d4b09008423308a15c69bf20f188df70d5d7c8045c4fb4f'],
                                              'native_agent_config_probe': ['agent-config-probe.py',
                                                                            'badabc801a8fa2fb6a9340489ad2c99972e6b011e6c972bec757990ae032608e'],
                                              'native_agent_config_ui': ['agent-config-ui.json',
                                                                         'cd2d8f9265e9758a3ba0d49828f82a8eb35a12de722252660a9a17e8dc992236'],
                                              'native_agent_native_entry_controller': ['agent-native-entry-controller.py',
                                                                                       '739940c925145f49136d7cc618a01f3b7775b4ae84cbb1d2647221032270f3e4'],
                                              'native_agent_native_bootstrap': ['agent-native-bootstrap.py',
                                                                                '4dfd9d275a5639e85a3ebce71fbaa620e1e3ca3f9982199887d04a65f6933439'],
                                              'native_agent_native_entry_action': ['agent-native-entry-action.py',
                                                                                   '3545a09ce95f583c6e738c3e18040cc0a671662304c366807d617af51f437617'],
                                              'native_agent_native_public_probe': ['agent-native-public-probe.py',
                                                                                   'c0d9f9e0fe477e9e1592c765c49d197aaf841da0e4ca92114f63f32d417ecfa2'],
                                              'native_public_probe_11ebf20_ui4': ['public_probe_11ebf20_ui4.py',
                                                                                  'ef194ed6b55e545c922308f875aed184d76490530c8f2a88a27459ce3f1994bd'],
                                              'native_public_action_11ebf20_ui4': ['public_action_11ebf20_ui4.py',
                                                                                   '8aafd63bc2653c1099003edbf5808c1c4550471d31f619cf1851a0e7ea063518']},
                                  'ui': ['reviewed-ui-11ebf20-a3.json',
                                         '3770f759f4479904651f3e3404e0ff23b6bea08c3a007320bf2d915ff7c93852'],
                                  'next_stage': 'canvas-entry',
                                  'artifacts': {'main-qa-agent-native-bootstrap': {'png': ['01-before-quick.png',
                                                                                           '02-after-quick-1280x900.png',
                                                                                           '03-canvas-gallery-1280x900.png',
                                                                                           '06-current-new-canvas-dialog.png',
                                                                                           '07-current-create-result.png'],
                                                                                   'public': ['06-current-new-canvas-dialog-public.json',
                                                                                              '07-create-result-public.json'],
                                                                                   'action': [],
                                                                                   'other': ['bootstrap.json']},
                                                'main-qa-agent-native-entry': {'png': ['01-before-hover.png',
                                                                                       '03-assistant-tooltip-review.png',
                                                                                       '04-assistant-panel-1280x900.png',
                                                                                       '05-assistant-panel-1024x900.png',
                                                                                       '06-assistant-panel-1440x900.png',
                                                                                       '07-assistant-panel-1280x720.png'],
                                                                               'public': ['01-before-hover-public.json',
                                                                                          '02-recheck-public.json',
                                                                                          '03-after-hover-public.json',
                                                                                          '03-before-open-public.json',
                                                                                          '04-panel-public.json',
                                                                                          '05-panel-1024x900-public.json',
                                                                                          '06-panel-1440x900-public.json',
                                                                                          '07-panel-1280x720-public.json'],
                                                                               'action': ['04-entry-action.json'],
                                                                               'other': ['agent-native-entry.json']},
                                                'main-qa-agent-config': {'png': [],
                                                                         'public': ['01-restored-panel-public.json',
                                                                                    '03-config-public.json',
                                                                                    '05-after-close-public.json'],
                                                                         'action': ['02-open-settings-action.json',
                                                                                    '04-close-assistant-action.json'],
                                                                         'other': ['agent-config.json']}}}})

NATIVE_SCOPES.update({'assets-import': {'scripts': {'qa': ['asset-clip-assets-controller.py',
                                      'c636176895cf36115c396854296ee57a4cff9d6fcaa1b95d588443f1235587e1'],
                               'native_public_probe': ['asset-clip-public-probe.py',
                                                       'a4c7f841a3bc091c6aa9fced8b9f7c9e04618997db32d8526169c80274067c38'],
                               'native_action': ['asset-clip-native-action.py',
                                                 'd06bfe6d3df84bb7d2145f69ff8c1180bb1a27875f694e0934816c26ee2abd46']},
                   'ui': ['asset-clip-assets-import-ui.json',
                          'ebc50ec52969e7e854c39e224810c574fd4c8576e8e41265576032e0ba428850'],
                   'next_stage': None,
                   'artifacts': {'main-qa-asset-clip-assets-import': {'png': ['01-after-quick.png',
                                                                              '02-page.png',
                                                                              '03-native-observed.png',
                                                                              '03-app-unknown-result.png',
                                                                              '04-native-location.png',
                                                                              '06-native-still-visible.png',
                                                                              '06-assets-import-result.png'],
                                                                      'public': ['01-after-quick-prepixels-public.json',
                                                                                 '01-after-quick-public.json',
                                                                                 '02-before-navigation-public.json',
                                                                                 '02-page-public.json',
                                                                                 '02-page-target-public.json',
                                                                                 '02-target-recheck-public.json',
                                                                                 '03-native-observed-public.json',
                                                                                 '03-app-unknown-result-public.json',
                                                                                 '04-native-before-public.json',
                                                                                 '04-native-location-public.json',
                                                                                 '05-native-location-public.json',
                                                                                 '05-native-before-accept-public.json',
                                                                                 '06-native-still-visible-public.json',
                                                                                 '06-assets-import-result-public.json'],
                                                                      'action': ['05-native-set-location-action.json',
                                                                                 '05-native-accept-action.json'],
                                                                      'other': ['asset-clip-report.json']}},
                   'artifact_JSON_limits': {'public_metadata_each': 131072,
                                            'native_action_each': 8192,
                                            'report_each': 16384},
                   'fixture': {'source': 'scripts/qa-fixtures/opaque-quadrants.png',
                               'destination_directory': 'asset-clip-inputs',
                               'basename': 'opaque-quadrants.png',
                               'bytes': 800,
                               'sha256': '0928c47fa44250879270def6198e04fd939dd8250864179760203f0d334a6d63',
                               'launcher_argument': ['--input-dir', '<work>/asset-clip-inputs'],
                               'copy_policy': 'Register and copy only for assets-import, mkdir0700 then '
                                              'exact regular fixture copy0600; validate only one file. Other '
                                              'native scopes keep fixture/input-dir forbidden.'}},
 'clip-create-observation': {'scripts': {'qa': ['asset-clip-clip-controller.py',
                                                '01003a99989cc03d986ae46eccef719d52c1cffd829451b4b8553bc5e1091e27'],
                                         'native_public_probe': ['public_probe_11ebf20_ui4.py',
                                                                 'ef194ed6b55e545c922308f875aed184d76490530c8f2a88a27459ce3f1994bd']},
                             'ui': ['asset-clip-clip-create-observation-ui.json',
                                    '2e00eb06a48e2c2069e9a7358e7b4149e22cd0027d027c728b55ceb012e7785b'],
                             'next_stage': None,
                             'artifacts': {'main-qa-asset-clip-clip-create-observation': {'png': ['01-after-quick.png',
                                                                                                  '02-page.png',
                                                                                                  '03-new-clip-dialog-observed.png'],
                                                                                          'public': ['01-after-quick-prepixels-public.json',
                                                                                                     '01-after-quick-public.json',
                                                                                                     '02-before-navigation-public.json',
                                                                                                     '02-page-public.json',
                                                                                                     '02-page-target-public.json',
                                                                                                     '02-target-recheck-public.json',
                                                                                                     '03-new-clip-dialog-observed-public.json'],
                                                                                          'action': [],
                                                                                          'other': ['asset-clip-report.json']}},
                             'artifact_JSON_limits': {'public_metadata_each': 131072,
                                                      'native_action_each': 8192,
                                                      'report_each': 16384}}})

WORKFLOW_FIXTURE_MANIFEST = "8926b97d3370005fa008bcac6cf21fc287d3717448968591b1d705df76a290a7"
WORKFLOW_FIXTURES = {
    "opaque-quadrants.png": (800, "0928c47fa44250879270def6198e04fd939dd8250864179760203f0d334a6d63"),
    "transparent-markers.png": (849, "8201fa2b0c7c94de97433462d40ae97aa43b77e395921769d967c7f168875b75"),
    "fully-transparent.png": (83, "ba04f531df0c7a12124750d521add77c55b16a6432d653c5559c16680dbd9f50"),
}

NATIVE_SCOPES.update({'clip-editor-entry': {'scripts': {'qa': ['clip-editor-controller.py',
                                          'd2309208c6be02feeaa618e3e80b31ac49bc3bd4a33e87b68ce2d8a1d68c48d2'],
                                   'clip_editor_controller': ['clip-editor-controller.py',
                                                              'd2309208c6be02feeaa618e3e80b31ac49bc3bd4a33e87b68ce2d8a1d68c48d2'],
                                   'native_public_probe': ['clip-editor-public-probe.py',
                                                           '562612f3bf1d54da2f9011fdea0849b6ecdf0dc66d129b42546fb0bd21bc3cd8'],
                                   'clip_editor_action': ['clip-editor-action.py',
                                                          '01c3b609e0ba012616df3a99ab0a5ea98629b32124b28cdd49489b609c5546ce']},
                       'ui': ['clip-editor-ui.json',
                              'bae028e170225c532126ff7cb62dd4e766ea763716e8871615b8d909fd019a90'],
                       'next_stage': None,
                       'artifacts': {'main-qa-clip-editor-entry': {'png': ['01-after-quick.png',
                                                                           '02-page.png',
                                                                           '03-current-new-clip-dialog.png',
                                                                           '07-editor-1280x900.png',
                                                                           '08-editor-1024x900.png',
                                                                           '09-editor-1440x900.png',
                                                                           '10-editor-1280x720.png',
                                                                           '11-unknown-editor-result.png'],
                                                                   'public': ['01-after-quick-prepixels-public.json',
                                                                              '01-after-quick-public.json',
                                                                              '02-before-navigation-public.json',
                                                                              '02-page-public.json',
                                                                              '02-page-target-public.json',
                                                                              '02-target-recheck-public.json',
                                                                              '03-current-new-clip-dialog-public.json',
                                                                              '03-dialog-recheck-public.json',
                                                                              '06-before-create-public.json',
                                                                              '07-editor-sample-01-public.json',
                                                                              '07-editor-sample-02-public.json',
                                                                              '07-editor-sample-03-public.json',
                                                                              '07-editor-sample-04-public.json',
                                                                              '07-editor-sample-05-public.json',
                                                                              '07-editor-sample-06-public.json',
                                                                              '07-editor-sample-07-public.json',
                                                                              '07-editor-sample-08-public.json',
                                                                              '07-editor-prepixels-public.json',
                                                                              '07-editor-1280x900-public.json',
                                                                              '08-editor-prepixels-public.json',
                                                                              '08-editor-1024x900-public.json',
                                                                              '09-editor-prepixels-public.json',
                                                                              '09-editor-1440x900-public.json',
                                                                              '10-editor-prepixels-public.json',
                                                                              '10-editor-1280x720-public.json',
                                                                              '11-unknown-editor-result-public.json'],
                                                                   'action': ['04-name-action.json',
                                                                              '05-path-action.json',
                                                                              '06-create-action.json'],
                                                                   'other': ['clip-editor-report.json']}}}})

NATIVE_SCOPES.update({'assistant-config-observation': {'scripts': {'qa': ['assistant-0aa/agent-config-controller.py',
                                                     '7c18962bbe13f8658eb0d36abcc198ec527ef645cf332d2c1fc21f2df411e69f'],
                                              'native_agent_config_controller': ['assistant-0aa/agent-config-controller.py',
                                                                                 '7c18962bbe13f8658eb0d36abcc198ec527ef645cf332d2c1fc21f2df411e69f'],
                                              'native_agent_config_action': ['assistant-0aa/agent-config-action.py',
                                                                             '125c29c64416311a7d4b09008423308a15c69bf20f188df70d5d7c8045c4fb4f'],
                                              'native_agent_config_probe': ['assistant-0aa/agent-config-probe.py',
                                                                            '6eefb42a02a7fdc2eee6febd4f47929ce5c15516d78a5bd405b49a205f4f9549'],
                                              'native_agent_config_ui': ['assistant-0aa/agent-config-ui.json',
                                                                         '601189cc0a6bdbe8481e4a1854147a6c96b37b33616cb37e9104ef608e060bd3'],
                                              'native_agent_native_entry_controller': ['assistant-0aa/agent-native-entry-controller.py',
                                                                                       '59cbed35f6b3d7748933030f633a12d23f80ae25bbb044fa1c7e024bd3d3bfaf'],
                                              'native_agent_native_bootstrap': ['assistant-0aa/agent-native-bootstrap.py',
                                                                                '6195749a38f4b1ccac0d17c9d413ea7e69e92af939916be25195b202a38e4529'],
                                              'native_agent_native_entry_action': ['assistant-0aa/agent-native-entry-action.py',
                                                                                   '3545a09ce95f583c6e738c3e18040cc0a671662304c366807d617af51f437617'],
                                              'native_agent_native_public_probe': ['assistant-0aa/agent-native-public-probe.py',
                                                                                   '20456d60ce8ca14f72941026e0b56e97eebbf552267d991d61bf3be7b867702f'],
                                              'native_public_probe_11ebf20_ui4': ['assistant-0aa/public_probe_11ebf20_ui4.py',
                                                                                  '93352469c112f8ab9144d2390a43fd282e746d37b3193c13ce90bd9e9d3c04f0'],
                                              'native_public_action_11ebf20_ui4': ['assistant-0aa/public_action_11ebf20_ui4.py',
                                                                                   '8aafd63bc2653c1099003edbf5808c1c4550471d31f619cf1851a0e7ea063518']},
                                  'ui': ['assistant-0aa/reviewed-ui-11ebf20-a3.json',
                                         '034c156538dba7ffd3aabf8bde0b25c3369d896490fbc88ac11b144a14e1f448'],
                                  'next_stage': 'canvas-entry',
                                  'artifacts': {'main-qa-agent-native-bootstrap': {'png': ['01-before-quick.png',
                                                                                           '02-after-quick-1280x900.png',
                                                                                           '03-canvas-gallery-1280x900.png',
                                                                                           '06-current-new-canvas-dialog.png',
                                                                                           '07-current-create-result.png'],
                                                                                   'public': ['06-current-new-canvas-dialog-public.json',
                                                                                              '07-create-result-public.json'],
                                                                                   'action': [],
                                                                                   'other': ['bootstrap.json']},
                                                'main-qa-agent-native-entry': {'png': ['01-before-hover.png',
                                                                                       '03-assistant-tooltip-review.png',
                                                                                       '04-assistant-panel-1280x900.png',
                                                                                       '05-assistant-panel-1024x900.png',
                                                                                       '06-assistant-panel-1440x900.png',
                                                                                       '07-assistant-panel-1280x720.png'],
                                                                               'public': ['01-before-hover-public.json',
                                                                                          '02-recheck-public.json',
                                                                                          '03-after-hover-public.json',
                                                                                          '03-before-open-public.json',
                                                                                          '04-panel-public.json',
                                                                                          '05-panel-1024x900-public.json',
                                                                                          '06-panel-1440x900-public.json',
                                                                                          '07-panel-1280x720-public.json'],
                                                                               'action': ['04-entry-action.json'],
                                                                               'other': ['agent-native-entry.json']},
                                                'main-qa-agent-config': {'png': [],
                                                                         'public': ['01-restored-panel-public.json',
                                                                                    '03-config-public.json',
                                                                                    '05-after-close-public.json'],
                                                                         'action': ['02-open-settings-action.json',
                                                                                    '04-close-assistant-action.json'],
                                                                         'other': ['agent-config.json']}}}})


PERSISTENCE_SCOPES = {'persistence-seed-observation', 'persistence-reopen-observation'}
PERSISTENCE_LIMIT = 7 * 1024 * 1024
PERSISTENCE_SCRIPTS = {'persistence-seed-observation': {'public_ui_probe': ('public_ui_probe.py', 'd6e74b62eaaca096ec75f55fd0326942dbece24dfb0e32397062c1248a3c23e2'), 'workflow_checks': ('workflow_checks.py', '93b6246ec2f0bb450753812bc0eacc0493480ab1bee9acccf88cd19210bfe853'), 'native_ui_action': ('style-native-ui-action.py', 'a89dcf2431a5a38a5b94fa9ffc537785ef54849623e66b6ae981c97ff441a45f'), 'persistence_state': ['persistence-state.py', '87a436a01e0161c41afbdd71a9a16523a1b95af686f6f348efba983ba490a48e'], 'capture_probe': ['public_probe_11ebf20_ui4.py', 'ef194ed6b55e545c922308f875aed184d76490530c8f2a88a27459ce3f1994bd'], 'qa': ['persistence-seed-controller.py', 'cbf555ae349f7a6b083e01cf9a1f5d747020ceb4dc4ef82f0428efe4567e96fa']}, 'persistence-reopen-observation': {'public_ui_probe': ('public_ui_probe.py', 'd6e74b62eaaca096ec75f55fd0326942dbece24dfb0e32397062c1248a3c23e2'), 'workflow_checks': ('workflow_checks.py', '93b6246ec2f0bb450753812bc0eacc0493480ab1bee9acccf88cd19210bfe853'), 'native_ui_action': ('style-native-ui-action.py', 'a89dcf2431a5a38a5b94fa9ffc537785ef54849623e66b6ae981c97ff441a45f'), 'persistence_state': ['persistence-state.py', '87a436a01e0161c41afbdd71a9a16523a1b95af686f6f348efba983ba490a48e'], 'capture_probe': ['public_probe_11ebf20_ui4.py', 'ef194ed6b55e545c922308f875aed184d76490530c8f2a88a27459ce3f1994bd'], 'qa': ['persistence-reopen-controller.py', 'dedc8f296714724a9d61fa4985f16ecb1b90b9a2daab84937575c7fc974fc034']}}
PERSISTENCE_JSON_NAMES = {'persistence-seed.json', 'persistence-prelaunch.json', 'persistence-postrun.json', 'persistence-failure.json'}

APPROVAL_LIMIT = 16 * 1024
APP_LIMIT = 512 * 1024 * 1024
HARNESS_RESERVE = 2 * LOG_LIMIT + 32 * 1024


class StopRun(Exception):
    pass


class BoundedLog:
    """Drain continuously; retain only complete redacted lines in bounded RAM."""

    def __init__(self, stream):
        self.stream = stream
        self.lines = deque()
        self.size = 0
        self.truncated = False
        self.lock = threading.Lock()
        self.thread = threading.Thread(target=self._read, daemon=True)
        self.thread.start()

    def _append(self, line):
        if SENSITIVE.search(line):
            line = b"[redacted sensitive line]\n"
        with self.lock:
            self.lines.append(line)
            self.size += len(line)
            while self.size > LOG_LIMIT:
                self.size -= len(self.lines.popleft())
                self.truncated = True

    def _read(self):
        pending = bytearray()
        oversized = False
        try:
            while True:
                chunk = self.stream.read(4096)
                if not chunk:
                    break
                for part in chunk.splitlines(keepends=True):
                    ends_line = part.endswith((b"\n", b"\r"))
                    if not oversized:
                        pending.extend(part)
                        if len(pending) > LINE_LIMIT:
                            pending.clear()
                            oversized = True
                    if ends_line:
                        self._append(b"[oversized line omitted]\n" if oversized else bytes(pending))
                        pending.clear()
                        oversized = False
            if oversized or pending:
                self._append(b"[oversized line omitted]\n" if oversized else bytes(pending) + b"\n")
        finally:
            self.stream.close()

    def snapshot(self):
        with self.lock:
            return b"".join(self.lines)


def regular_input(value):
    path = Path(value).absolute()
    mode = path.lstat().st_mode
    if not stat.S_ISREG(mode):
        raise ValueError("inputs must be regular files, not symlinks")
    return path


def fresh_output(value):
    path = Path(value)
    if not path.is_absolute():
        raise ValueError("--output must be absolute")
    if ".." in path.parts:
        raise ValueError("--output must not contain parent traversal")
    for parent in reversed(path.parents):
        if parent.is_symlink() or not parent.is_dir():
            raise ValueError("--output ancestors must be existing real directories")
    path.mkdir(mode=0o700)  # Existing paths, including dangling symlinks, fail.
    return path


def copy_and_hash(source, destination, executable=False, limit=None):
    digest = hashlib.sha256()
    fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as incoming, destination.open("xb") as outgoing:
        if not stat.S_ISREG(os.fstat(incoming.fileno()).st_mode):
            raise ValueError("input changed to a non-regular file")
        copied_bytes = 0
        while True:
            chunk = incoming.read(1024 * 1024)
            if not chunk:
                break
            copied_bytes += len(chunk)
            if limit is not None and copied_bytes > limit:
                raise ValueError("input exceeds its copy budget")
            digest.update(chunk)
            outgoing.write(chunk)
    destination.chmod(0o700 if executable else 0o600)
    return digest.hexdigest()


def copy_workflow_fixtures(source, work):
    """Copy only the reviewed owned fixture inputs, never an arbitrary tree."""
    if not source.is_absolute() or ".." in source.parts:
        raise ValueError("--input-dir must be an absolute owned fixture directory")
    for path in (source, *source.parents):
        if not stat.S_ISDIR(path.lstat().st_mode):
            raise ValueError("fixture directory ancestors must be real directories")
    if source.stat().st_uid != os.getuid():
        raise ValueError("fixture directory must be owned by this runner")
    if {item.name for item in source.iterdir()} != {"manifest.json", *WORKFLOW_FIXTURES}:
        raise ValueError("fixture directory must contain exactly the reviewed four inputs")
    destination = work / "qa-input"
    destination.mkdir(mode=0o700)
    hashes = {}
    for name in ("manifest.json", *WORKFLOW_FIXTURES):
        incoming = regular_input(source / name)
        info = incoming.lstat()
        if info.st_uid != os.getuid() or info.st_nlink != 1:
            raise ValueError("fixture inputs must be owned regular files with one link")
        maximum = APPROVAL_LIMIT if name == "manifest.json" else WORKFLOW_FIXTURES[name][0]
        digest = copy_and_hash(incoming, destination / name, limit=maximum)
        expected = WORKFLOW_FIXTURE_MANIFEST if name == "manifest.json" else WORKFLOW_FIXTURES[name][1]
        if digest != expected or (name != "manifest.json" and (destination / name).stat().st_size != maximum):
            raise ValueError("copied fixture differs from its reviewed identity")
        hashes[name] = digest
    return destination, hashes


NATIVE_SCOPES.update({'asset-library-flow': {'scripts': {'qa': ['asset-flow-controller.py',
                                           '76c0d482d4e56f800c21119ee1edb4c6be66b0b57d777ad843969ba12197537c'],
                                    'native_public_probe': ['asset-flow-public-probe.py',
                                                            '2f965552da7b5fe4c5fd002e010a2c712091c441d87bef2e8cabb2f0fe52c17d'],
                                    'native_action': ['asset-flow-native-action.py',
                                                      '63704d9b66fa6e4bcb01cbe4e3a815b7129c61d31abea994a77124986d4cf125'],
                                    'asset_app_action': ['asset-flow-app-action.py',
                                                         '557ff54d7285d4d5bd10db4cccfe4412fab6561f3088316a0e9c64d33b28f757']},
                        'ui': ['asset-flow-ui.json',
                               '0967ff7b555e50a51e476ec5105600b0472fa9cfe36e522bb1a5f8500b4e8d84'],
                        'next_stage': None,
                        'artifacts': {'main-qa-asset-library-flow': {'png': [
                                                                             '02-page.png',
                                                                             '03-native-observed.png',
                                                                             '03-app-unknown-result.png',
                                                                             '04-native-location.png',
                                                                             '06-native-still-visible.png',
                                                                             '06-assets-import-result.png',
                                                                             '07-search-empty.png',
                                                                             '08-search-restored.png',
                                                                             '09-single-selected.png',
                                                                             '10-canvas-flow-result.png',
                                                                             '10-unknown-flow-result.png',
                                                                             '11-handoff-new-result.png', '12-canvas-result.png', '13-before-return-assets-requires-review.png', '13-assets-return-result.png'],
                                                                     'public': ['01-after-quick-prepixels-public.json',
                                                                                '01-after-quick-public.json',
                                                                                '02-before-navigation-public.json',
                                                                                '02-page-public.json',
                                                                                '02-page-target-public.json',
                                                                                '02-target-recheck-public.json',
                                                                                '03-native-observed-public.json',
                                                                                '03-app-unknown-result-public.json',
                                                                                '04-native-before-public.json',
                                                                                '04-native-location-public.json',
                                                                                '05-native-location-public.json',
                                                                                '05-native-before-accept-public.json',
                                                                                '06-native-still-visible-public.json',
                                                                                '06-assets-import-result-public.json',
                                                                                '07-before-search-public.json',
                                                                                '07-search-empty-recheck-public.json',
                                                                                '07-search-empty-public.json',
                                                                                '08-before-clear-public.json',
                                                                                '08-restored-recheck-public.json',
                                                                                '08-search-restored-public.json',
                                                                                '09-before-batch-public.json',
                                                                                '09-batch-zero-public.json',
                                                                                '09-selected-recheck-public.json',
                                                                                '09-single-selected-public.json',
                                                                                '10-before-canvas-public.json',
                                                                                '10-canvas-flow-result-public.json',
                                                                                '10-unknown-flow-result-public.json',
                                                                                '11-before-handoff-new-public.json',
                                                                                '11-handoff-new-result-public.json', '11-handoff-open-recheck-public.json', '12-canvas-result-public.json', '13-before-return-assets-public.json', '13-return-assets-recheck-public.json', '13-before-return-assets-requires-review-public.json', '13-assets-return-result-public.json'],
                                                                     'action': ['05-native-set-location-action.json',
                                                                                '05-native-accept-action.json',
                                                                                '07-search-miss-action.json',
                                                                                '08-search-clear-action.json',
                                                                                '09-batch-action.json',
                                                                                '09-select-action.json',
                                                                                '10-canvas-action.json',
                                                                                '11-handoff-new-action.json', '12-handoff-open-action.json'],
                                                                     'other': ['asset-flow-report.json']}},
                        'artifact_JSON_limits': {'public_metadata_each': 131072,
                                                 'native_action_each': 8192,
                                                 'report_each': 16384},
                        'fixture': {'source': 'scripts/qa-fixtures/opaque-quadrants.png',
                                    'destination_directory': 'asset-clip-inputs',
                                    'basename': 'opaque-quadrants.png',
                                    'bytes': 800,
                                    'sha256': '0928c47fa44250879270def6198e04fd939dd8250864179760203f0d334a6d63',
                                    'launcher_argument': ['--input-dir', '<work>/asset-clip-inputs'],
                                    'copy_policy': 'Register only asset-library-flow in addition to the '
                                                   'existing assets-import scope; reuse exact one regular '
                                                   '800B PNG copy0600 in owned0700 asset-clip-inputs; fixed '
                                                   'checked-out scripts/qa-fixtures source. All other native '
                                                   'scopes keep input-dir forbidden.'}}})


# Exact reviewed clip import controller and pinned nonfield templates.
NATIVE_SCOPES.update({'clip-media-import': {'scripts': {'qa': ['clip-import-controller.py',
                                          'eb00c397adb4e9372d3d0d609f9dca3a13b0c4ce8209ad48daecb324536180c0'],
                                   'clip_import_controller': ['clip-import-controller.py',
                                                              'eb00c397adb4e9372d3d0d609f9dca3a13b0c4ce8209ad48daecb324536180c0'],
                                   'native_public_probe': ['clip-editor-public-probe.py',
                                                           '562612f3bf1d54da2f9011fdea0849b6ecdf0dc66d129b42546fb0bd21bc3cd8'],
                                   'clip_editor_action': ['clip-editor-action.py',
                                                          '01c3b609e0ba012616df3a99ab0a5ea98629b32124b28cdd49489b609c5546ce'],
                                   'native_action': ['clip-import-native-action.py',
                                                     '7190a3d65580f90251d45cfeb6ae0c00f8cc9802a725fa941aac6394b2a525d0'],
                                   'clip_import_guard': ['clip-import-guard.json',
                                                         'b4cf31bf64a25144069869d2122834ba00e7784db3940f8376e955eed38d543b'],
                                   'clip_timeline_guard': ['clip-timeline-guard.json',
                                                           'fac01c381f3a4fdadfbf2131ad89853c8885fa67d676bccf998c4ffed248ad10'],
                                   'clip_post_insert': ['clip-post-insert.py',
                                                        '2db0ba8c66f238353ec8ad2177f7ea19f2be08a3913f730cdb8e1b0f972f498f']},
                       'ui': ['clip-import-ui.json',
                              'c6fb4d06e05ec2e941e951096ecc149fee59c11b1a629fd35417ce5c0cb320d5'],
                       'next_stage': None,
                       'artifacts': {'main-qa-clip-media-import': {'png': ['01-after-quick.png',
                                                                           '02-page.png',
                                                                           '03-current-new-clip-dialog.png',
                                                                           '07-editor-1280x900.png',
                                                                           '08-editor-1024x900.png',
                                                                           '09-editor-1440x900.png',
                                                                           '10-editor-1280x720.png',
                                                                           '11-unknown-editor-result.png',
                                                                           '12-import-native-observed.png',
                                                                           '12-import-app-unknown-result.png',
                                                                           '13-import-native-location.png',
                                                                           '15-import-result.png',
                                                                           '16-timeline-result.png',
                                                                           '17-move-result.png',
                                                                           '18-undo-result.png',
                                                                           '19-save-result.png',
                                                                           '20-file-menu-observed.png'],
                                                                   'public': ['01-after-quick-prepixels-public.json',
                                                                              '01-after-quick-public.json',
                                                                              '02-before-navigation-public.json',
                                                                              '02-page-public.json',
                                                                              '02-page-target-public.json',
                                                                              '02-target-recheck-public.json',
                                                                              '03-current-new-clip-dialog-public.json',
                                                                              '03-dialog-recheck-public.json',
                                                                              '06-before-create-public.json',
                                                                              '07-editor-sample-01-public.json',
                                                                              '07-editor-sample-02-public.json',
                                                                              '07-editor-sample-03-public.json',
                                                                              '07-editor-sample-04-public.json',
                                                                              '07-editor-sample-05-public.json',
                                                                              '07-editor-sample-06-public.json',
                                                                              '07-editor-sample-07-public.json',
                                                                              '07-editor-sample-08-public.json',
                                                                              '07-editor-prepixels-public.json',
                                                                              '07-editor-1280x900-public.json',
                                                                              '08-editor-prepixels-public.json',
                                                                              '08-editor-1024x900-public.json',
                                                                              '09-editor-prepixels-public.json',
                                                                              '09-editor-1440x900-public.json',
                                                                              '10-editor-prepixels-public.json',
                                                                              '10-editor-1280x720-public.json',
                                                                              '11-unknown-editor-result-public.json',
                                                                              '11-before-import-public.json',
                                                                              '11-import-target-recheck-public.json',
                                                                              '12-import-native-observed-public.json',
                                                                              '12-import-native-sample-01-public.json',
                                                                              '12-import-native-sample-02-public.json',
                                                                              '12-import-native-sample-03-public.json',
                                                                              '12-import-native-before-public.json',
                                                                              '12-import-app-unknown-result-public.json',
                                                                              '13-import-native-location-public.json',
                                                                              '13-import-native-location-recheck-public.json',
                                                                              '14-before-set-location-public.json',
                                                                              '14-native-before-accept-public.json',
                                                                              '14-before-accept-public.json',
                                                                              '15-import-result-public.json',
                                                                              '15-import-result-sample-01-public.json',
                                                                              '15-import-result-sample-02-public.json',
                                                                              '15-import-result-sample-03-public.json',
                                                                              '15-import-result-sample-04-public.json',
                                                                              '16-before-insert-public.json',
                                                                              '16-timeline-result-public.json',
                                                                              '16-timeline-result-sample-01-public.json',
                                                                              '16-timeline-result-sample-02-public.json',
                                                                              '16-timeline-result-sample-03-public.json',
                                                                              '16-timeline-result-sample-04-public.json',
                                                                              '17-before-move-public.json',
                                                                              '18-before-undo-public.json',
                                                                              '19-before-save-public.json',
                                                                              '20-before-file-public.json',
                                                                              '17-move-result-public.json',
                                                                              '17-move-result-sample-01-public.json',
                                                                              '17-move-result-sample-02-public.json',
                                                                              '17-move-result-sample-03-public.json',
                                                                              '17-move-result-sample-04-public.json',
                                                                              '18-undo-result-public.json',
                                                                              '18-undo-result-sample-01-public.json',
                                                                              '18-undo-result-sample-02-public.json',
                                                                              '18-undo-result-sample-03-public.json',
                                                                              '18-undo-result-sample-04-public.json',
                                                                              '19-save-result-public.json',
                                                                              '19-save-result-sample-01-public.json',
                                                                              '19-save-result-sample-02-public.json',
                                                                              '19-save-result-sample-03-public.json',
                                                                              '19-save-result-sample-04-public.json',
                                                                              '20-file-menu-observed-public.json',
                                                                              '20-file-menu-observed-sample-01-public.json',
                                                                              '20-file-menu-observed-sample-02-public.json',
                                                                              '20-file-menu-observed-sample-03-public.json',
                                                                              '20-file-menu-observed-sample-04-public.json'],
                                                                   'action': ['04-name-action.json',
                                                                              '05-path-action.json',
                                                                              '06-create-action.json',
                                                                              '14-native-set-location-action.json',
                                                                              '14-native-accept-action.json'],
                                                                   'other': ['clip-import-report.json']}},
                       'fixture': {'source': 'scripts/qa-fixtures/opaque-quadrants.png',
                                   'bytes': 800,
                                   'sha256': '0928c47fa44250879270def6198e04fd939dd8250864179760203f0d334a6d63',
                                   'destination_directory': 'asset-clip-inputs',
                                   'copy_policy': 'Reuse copy_assets_fixture only for this explicit scope; '
                                                  'source must be checked-out scripts/qa-fixtures; copy0700 '
                                                  'directory and0600 file; exactly one file; controller '
                                                  'checks work-dir/asset-clip-inputs.'}}})


def copy_assets_fixture(source, work):
    """Only assets-import receives one copied synthetic PNG."""
    if (not source.is_absolute() or source.is_symlink() or not source.is_dir()
            or source.stat().st_uid != os.getuid()):
        raise ValueError('exact owned fixture directory required')
    fixture = source / 'opaque-quadrants.png'
    info = fixture.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
            or info.st_uid != os.getuid() or info.st_size != 800):
        raise ValueError('exact ordinary 800-byte assets fixture required')
    destination = work / 'asset-clip-inputs'
    destination.mkdir(mode=0o700)
    digest = copy_and_hash(fixture, destination / fixture.name, limit=800)
    expected = NATIVE_SCOPES['assets-import']['fixture']['sha256']
    if digest != expected:
        raise ValueError('assets fixture SHA256 changed')
    return destination, {fixture.name: digest}


def spawn(command, work, env):
    return subprocess.Popen(command, cwd=work, env=env, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            start_new_session=True)


def kill_group(process, signum):
    try:
        os.killpg(process.pid, signum)
    except ProcessLookupError:
        pass


def group_exists(process):
    try:
        os.killpg(process.pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        # EPERM also establishes existence (e.g. a transient macOS zombie in
        # helper self-checks). Keep waiting/reaping rather than skipping cleanup.
        return True


def stop_processes(processes, deadline):
    # Signal even exited leaders: their descendants may still own pipes.
    for process in processes:
        kill_group(process, signal.SIGTERM)
    grace = min(deadline, time.monotonic() + 5)
    while time.monotonic() < grace:
        for process in processes:
            process.poll()  # Reap a leader without forgetting its group.
        if not any(group_exists(process) for process in processes):
            break
        time.sleep(min(0.05, max(0, grace - time.monotonic())))
    for process in processes:
        kill_group(process, signal.SIGKILL)
    for process in processes:
        process.wait(timeout=max(0.01, deadline - time.monotonic()))


def inspect_artifacts(output, limit=ARTIFACT_LIMIT, next_stage=None):
    """Traverse only the QA contract's known directory, never links."""
    if not stat.S_ISDIR(output.lstat().st_mode):
        raise ValueError("output root must remain a real directory")
    persistence = next_stage in PERSISTENCE_SCOPES
    if persistence:
        limit = min(limit, PERSISTENCE_LIMIT)
    total = 0
    alpha = next_stage == ALPHA_STAGE
    workflow = next_stage == "workflow-observation" or persistence
    fresh = next_stage == "fresh-workbench-observation"
    native = NATIVE_SCOPES.get(next_stage)
    a2_scope = A2_SCOPES.get(next_stage)
    directories = ({A2_DIRECTORIES[next_stage]} if a2_scope else
                   ({NEXT_UI_DIRECTORY} if fresh else ({WORKFLOW_DIRECTORY} if workflow else QA_DIRECTORIES)))
    if alpha:
        directories = {ALPHA_DIRECTORY}
    if native:
        directories = set(native["artifacts"])
    png_count = png_bytes = project_count = alpha_json_count = 0
    pending = [output]
    while pending:
        directory = pending.pop()
        with os.scandir(directory) as entries:
            for entry in entries:
                info = entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(info.st_mode) and directory == output and entry.name in directories:
                    pending.append(Path(entry.path))
                    continue
                if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                    raise ValueError("output contains an unknown directory, link, or non-regular file")
                if native:
                    if directory == output:
                        if entry.name not in RESERVED:
                            raise ValueError("native artifacts require this scope's known QA directory")
                    else:
                        allowed = native["artifacts"][directory.name]
                        if entry.name in allowed["png"]:
                            png_count += 1
                            png_bytes += info.st_size
                            if png_count > 10 or info.st_size > WORKFLOW_FILE_LIMIT or png_bytes > WORKFLOW_PNG_LIMIT:
                                raise ValueError("native PNG count or byte budget exceeded")
                        elif entry.name in allowed["action"]:
                            if info.st_size > A2_ACTION_LIMIT:
                                raise ValueError("native action record exceeds 8 KiB")
                        elif entry.name in allowed["public"]:
                            if info.st_size > WORKFLOW_METADATA_LIMIT:
                                raise ValueError("native metadata exceeds 128 KiB")
                        elif entry.name in allowed.get("other", []):
                            if info.st_size > APPROVAL_LIMIT:
                                raise ValueError("native report exceeds 16 KiB")
                        else:
                            raise ValueError("unknown native scope artifact file")
                elif a2_scope:
                    if directory == output:
                        if entry.name not in RESERVED:
                            raise ValueError("A2 artifacts require this scope's known QA directory")
                    elif entry.name in A2_PNG_NAMES[a2_scope]:
                        png_count += 1
                        png_bytes += info.st_size
                        if png_count > 10 or info.st_size > WORKFLOW_FILE_LIMIT or png_bytes > WORKFLOW_PNG_LIMIT:
                            raise ValueError("A2 PNG count or byte budget exceeded")
                    elif entry.name in A2_ACTION_JSON_NAMES[a2_scope]:
                        if info.st_size > A2_ACTION_LIMIT:
                            raise ValueError("A2 action record exceeds 8 KiB")
                    elif entry.name == "a3.json" or entry.name in A2_PUBLIC_JSON_NAMES[a2_scope]:
                        if info.st_size > WORKFLOW_METADATA_LIMIT:
                            raise ValueError("A2 metadata exceeds 128 KiB")
                    else:
                        raise ValueError("unknown A2 scope artifact file")
                elif alpha:
                    if directory == output:
                        if entry.name not in RESERVED:
                            raise ValueError('alpha artifacts require the dedicated workflow directory')
                    elif entry.name in ALPHA_PNG_NAMES:
                        png_count += 1
                        png_bytes += info.st_size
                        if png_count > 40 or info.st_size > WORKFLOW_FILE_LIMIT or png_bytes > WORKFLOW_PNG_LIMIT:
                            raise ValueError('alpha workflow PNG count or byte budget exceeded')
                    elif entry.name in ALPHA_JSON_NAMES:
                        alpha_json_count += 1
                        if alpha_json_count > 128 or info.st_size > WORKFLOW_METADATA_LIMIT:
                            raise ValueError('alpha workflow JSON count or byte budget exceeded')
                    else:
                        raise ValueError('unknown alpha workflow artifact filename')
                elif workflow:
                    if directory == output:
                        if entry.name not in RESERVED:
                            raise ValueError("workflow artifacts require the known QA directory")
                    elif entry.name in WORKFLOW_PNG_NAMES or entry.name == "exported-qa.png":
                        png_count += entry.name != "exported-qa.png"
                        png_bytes += info.st_size
                        if png_count > 40 or info.st_size > WORKFLOW_FILE_LIMIT or png_bytes > WORKFLOW_PNG_LIMIT:
                            raise ValueError("workflow PNG count or byte budget exceeded")
                    elif entry.name in WORKFLOW_JSON_NAMES or (persistence and (
                            entry.name in PERSISTENCE_JSON_NAMES
                            or any(entry.name == prefix + name[:-4].lower() + '.json'
                                   for prefix in ('capture-', 'native-proof-')
                                   for name in WORKFLOW_PNG_NAMES))):
                        if info.st_size > WORKFLOW_METADATA_LIMIT:
                            raise ValueError("workflow metadata exceeds 128 KiB")
                    elif re.fullmatch(r"saved-project(?:\.[A-Za-z0-9_-]{1,16})?", entry.name):
                        project_count += 1
                        if project_count > 1 or info.st_size > WORKFLOW_FILE_LIMIT:
                            raise ValueError("workflow requires at most one bounded saved project")
                    else:
                        raise ValueError("unknown workflow artifact file")
                elif fresh:
                    if directory == output:
                        if entry.name not in RESERVED:
                            raise ValueError("fresh workbench artifacts require the known QA directory")
                    elif entry.name in NEXT_UI_PNG_NAMES:
                        png_count += 1
                        png_bytes += info.st_size
                        if png_count > 7 or info.st_size > WORKFLOW_FILE_LIMIT or png_bytes > WORKFLOW_PNG_LIMIT:
                            raise ValueError("fresh workbench PNG count or byte budget exceeded")
                    elif entry.name in NEXT_UI_JSON_NAMES:
                        if info.st_size > WORKFLOW_METADATA_LIMIT:
                            raise ValueError("fresh workbench metadata exceeds 128 KiB")
                    else:
                        raise ValueError("unknown fresh workbench artifact file")
                total += info.st_size
                if total > limit:
                    raise ValueError("output exceeds artifact budget")
    return total


def wait_for_window(app, work, env, deadline):
    if shutil.which("xdotool") is None:
        raise ValueError("xdotool is required for visible-window readiness")
    until = min(deadline, time.monotonic() + 60)
    while time.monotonic() < until:
        if app.poll() is not None:
            raise ValueError("App exited before one visible window became ready")
        probe = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(app.pid)],
                               cwd=work, env=env, capture_output=True, text=True,
                               timeout=min(5, max(0.01, until - time.monotonic())))
        if probe.returncode not in (0, 1):
            raise ValueError("visible-window search failed")
        windows = sorted(set(int(value) for value in probe.stdout.split()))
        if len(windows) > 1:
            raise ValueError("multiple visible App windows prevent unique identification")
        if windows:
            return windows[0]
        time.sleep(min(0.25, max(0, until - time.monotonic())))
    raise ValueError("no unique visible App window within readiness deadline")


def write_owned(output, name, payload):
    # Never follow or overwrite anything the QA has placed at a reserved path.
    fd = os.open(output / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as handle:
        handle.write(payload)


def verify_private_accessibility_bus(env):
    """Only the workflow's direct dbus-run-session child can attest this bus."""
    if not env.get("DBUS_SESSION_BUS_ADDRESS"):
        raise ValueError("private accessibility bus requires DBUS_SESSION_BUS_ADDRESS")
    parent = Path("/proc") / str(os.getppid())
    if (parent.stat().st_uid != os.getuid()
            or (parent / "exe").resolve(strict=True).name != "dbus-run-session"):
        raise ValueError("private accessibility bus requires a direct dbus-run-session parent")


def enable_private_accessibility(env, deadline, result):
    """Prepare only the attested fresh-stage bus before any App process exists."""
    state = {"status": "unavailable", "before": None, "after": None,
             "screen_reader_enabled": None}
    result["accessibility_preparation"] = state
    left = deadline - time.monotonic()
    if left <= 0:
        state["status"] = "deadline"
        raise ValueError("private accessibility preparation deadline")
    program = '''import json
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib
state = {"status": "unavailable", "before": None, "after": None, "screen_reader_enabled": None}
try:
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    def get(name):
        reply = bus.call_sync("org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties", "Get",
            GLib.Variant("(ss)", ("org.a11y.Status", name)), GLib.VariantType.new("(v)"),
            Gio.DBusCallFlags.NONE, 1000, None)
        value = reply.unpack()[0]
        if type(value) is not bool:
            raise ValueError("non_boolean")
        return value
    state["before"] = get("IsEnabled")
    state["screen_reader_enabled"] = get("ScreenReaderEnabled")
    bus.call_sync("org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties", "Set",
        GLib.Variant("(ssv)", ("org.a11y.Status", "IsEnabled", GLib.Variant("b", True))),
        GLib.VariantType.new("()"), Gio.DBusCallFlags.NONE, 1000, None)
    state["after"] = get("IsEnabled")
    state["status"] = "verified" if state["after"] is True else "not_enabled"
except Exception:
    pass
print(json.dumps(state))
'''
    try:
        completed = subprocess.run(["/usr/bin/python3", "-B", "-c", program], env=env,
                                   capture_output=True, timeout=min(5, left), check=False)
    except subprocess.TimeoutExpired:
        state["status"] = "timeout"
        raise ValueError("private accessibility preparation timed out") from None
    except OSError:
        raise ValueError("private accessibility preparation unavailable") from None
    try:
        data = json.loads(completed.stdout) if len(completed.stdout) <= 1024 else None
        if (completed.returncode != 0 or not isinstance(data, dict) or set(data) != set(state)
                or data["status"] not in {"verified", "unavailable", "not_enabled"}
                or any(data[key] is not None and type(data[key]) is not bool
                       for key in ("before", "after", "screen_reader_enabled"))):
            raise ValueError("invalid preparation result")
        state.update(data)
    except (ValueError, TypeError):
        raise ValueError("private accessibility preparation unavailable") from None
    if time.monotonic() >= deadline:
        state["status"] = "deadline"
        raise ValueError("private accessibility preparation deadline")
    if (state["status"] != "verified" or state["after"] is not True
            or type(state["before"]) is not bool or type(state["screen_reader_enabled"]) is not bool):
        raise ValueError("private accessibility preparation not verified")


def qa_command(args, copied, app_pid, window_id, work, output, source_head, qa_deadline):
    command = [sys.executable, str(copied["qa"]), "--app-pid", str(app_pid),
               "--work-dir", str(work), "--output", str(output),
               "--deadline-monotonic", str(qa_deadline)]
    if args.next_stage in ("workflow-observation", ALPHA_STAGE) or args.next_stage in PERSISTENCE_SCOPES:
        command.extend(("--window-id", str(window_id), "--input-dir", str(copied["input_dir"])))
        if args.private_accessibility_bus:
            command.extend(("--private-accessibility-bus", "--probe-python", "/usr/bin/python3"))
    elif args.next_stage == "fresh-workbench-observation":
        command.extend(("--window-id", str(window_id), "--expected-sha", source_head,
                        "--next-stage", "navigation", "--private-accessibility-bus",
                        "--probe-python", "/usr/bin/python3"))
    elif args.next_stage in NATIVE_SCOPES:
        native = NATIVE_SCOPES[args.next_stage]
        command[command.index("--deadline-monotonic") + 1] = str(
            min(qa_deadline, time.monotonic() + NATIVE_CONTROLLER_SECONDS + 15))
        command.extend(("--window-id", str(window_id), "--expected-sha", source_head,
                        "--private-accessibility-bus", "--probe-python", "/usr/bin/python3"))
        if native["next_stage"] is not None:
            command.extend(("--next-stage", native["next_stage"]))
        if args.next_stage in ('assets-import', 'asset-library-flow', 'clip-media-import'):
            command.extend(('--input-dir', str(copied['input_dir'])))
    elif args.next_stage in A2_SCOPES:
        command[command.index("--deadline-monotonic") + 1] = str(
            min(qa_deadline, time.monotonic() + A2_CONTROLLER_SECONDS + 15))
        command.extend(("--window-id", str(window_id), "--expected-sha", source_head,
                        "--next-stage", A2_SCOPES[args.next_stage], "--private-accessibility-bus",
                        "--probe-python", "/usr/bin/python3"))
    else:
        command.extend(("--client-binary", str(copied["client"]),
                        "--expected-sha", source_head, "--next-stage", args.next_stage))
    if args.next_stage in PERSISTENCE_SCOPES:
        command.extend(('--owned-state-root', args.owned_state_root, '--state-token', args.state_token))
        if args.next_stage == 'persistence-reopen-observation':
            command.extend(('--seed-record', args.seed_record))
    if args.isolated_display_capture:
        command.append("--isolated-display-capture")
    for name in ("identity_approval", "ui_approval"):
        if name in copied:
            command.extend(("--" + name.replace("_", "-"), str(copied[name])))
    return command


def persistence_helpers():
    import runpy
    source = Path(__file__).with_name('persistence-state.py')
    if hashlib.sha256(source.read_bytes()).hexdigest() != PERSISTENCE_SCRIPTS['persistence-seed-observation']['persistence_state'][1]:
        raise ValueError('persistence state verifier raw identity changed')
    return runpy.run_path(str(source))


def prepare_persistence_state(args, deadline):
    helpers = persistence_helpers()
    state = Path(args.owned_state_root)
    if (not state.is_absolute() or '..' in state.parts
            or state.name != 'seecut-persistence-' + args.state_token):
        raise ValueError('exact_random_batch_state_root_required')
    if args.next_stage == 'persistence-seed-observation':
        helpers['real_directory'](state.parent)
        state.mkdir(mode=0o700)  # Exclusive; existing directory or any link fails.
        info = helpers['real_directory'](state, private=True)
        marker = {'schema': 1, 'batch': args.state_token, 'uid': os.getuid(),
                  'source_head': WORKFLOW_HEAD, 'app_sha256': WORKFLOW_APP_SHA,
                  'root_device': info.st_dev, 'root_inode': info.st_ino}
        write_owned(state, 'persistence-batch.json', (json.dumps(marker) + '\n').encode())
    else:
        helpers['validate_batch'](state, args.state_token)
        # Already verify the previous process before reading binary or starting App.
        helpers['real_directory'](Path(args.seed_record).parent, private=True)
        record = json.loads(helpers['read_owned'](Path(args.seed_record), 32768))
        helpers['assert_process_gone'](record.get('app_pid'))
        if record.get('batch') != args.state_token or record.get('root') != str(state):
            raise ValueError('seed_record_batch_or_original_path_changed')
    return state


def hash_persistence_app(path, deadline):
    # B reuses the original inode and reads it only. No second executable copy.
    info = path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_nlink != 1 or info.st_size > APP_LIMIT or stat.S_IMODE(info.st_mode) != 0o700):
        raise ValueError('original_owned_App_identity_changed')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    digest = hashlib.sha256()
    with os.fdopen(fd, 'rb') as stream:
        opened = os.fstat(stream.fileno())
        if (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino):
            raise ValueError('original_App_changed_before_read')
        while True:
            if time.monotonic() >= deadline:
                raise ValueError('original_App_hash_deadline')
            chunk = stream.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    after = path.lstat()
    if (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns) != (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns):
        raise ValueError('original_App_changed_during_read')
    return digest.hexdigest()


def persistence_prelaunch(args, deadline):
    helpers = persistence_helpers()
    seed = json.loads(helpers['read_owned'](Path(args.seed_record), 32768))
    if (seed.get('schema') != 1 or seed.get('phase') != 'seed'
            or seed.get('batch') != args.state_token or seed.get('root') != args.owned_state_root
            or seed.get('app_sha256') != WORKFLOW_APP_SHA or seed.get('groups_gone') is not True):
        raise ValueError('complete_exact_seed_exit_record_required')
    helpers['assert_process_gone'](seed.get('app_pid'))
    app_info = (Path(args.owned_state_root) / 'concat').lstat()
    if seed.get('app_inode') != [app_info.st_dev, app_info.st_ino]:
        raise ValueError('original_App_inode_changed')
    state = helpers['snapshot_state'](Path(args.owned_state_root), args.state_token, deadline)
    if state != seed.get('state'):
        raise ValueError('saved_state_changed_between_A_exit_and_B_launch')
    second = helpers['snapshot_state'](Path(args.owned_state_root), args.state_token, deadline)
    if second != state:
        raise ValueError('saved_state_not_stable_before_B_launch')
    return {'schema': 1, 'batch': args.state_token, 'root': args.owned_state_root,
            'seed_app_pid': seed['app_pid'], 'seed_group_gone': True,
            'same_App_inode': True, 'unchanged_seed_state': True,
            'state_sha256': hashlib.sha256(json.dumps(state, sort_keys=True, separators=(',', ':')).encode()).hexdigest()}


def persistence_after_exit(args, result, processes, output, deadline):
    helpers = persistence_helpers()
    for process in processes:
        helpers['assert_process_gone'](process.pid)
    data = json.loads(helpers['read_owned'](output / WORKFLOW_DIRECTORY / 'workflow.json', 128 * 1024))
    export = data.get('stages', {}).get('export', {})
    if (data.get('status') != 'bounded_UI_observation_completed_review_pending'
            or export.get('exact_fixture_pixels_preserved') is not True
            or data.get('fixture_source_unchanged') is not True
            or data.get('settings_entered') is not False
            or data.get('app_pid') != result.get('app_pid')):
        raise ValueError('complete_UI_export_and_source_checks_required')
    scope = 'seed' if args.next_stage == 'persistence-seed-observation' else 'reopen'
    if data.get('persistence_scope') != scope or not data.get('stages', {}).get('16-gallery-reopened-edit', {}).get('one_Undo_restored'):
        raise ValueError('real_reopened_drag_and_one_Undo_required')
    state = helpers['snapshot_state'](Path(args.owned_state_root), args.state_token, deadline)
    if helpers['snapshot_state'](Path(args.owned_state_root), args.state_token, deadline) != state:
        raise ValueError('saved_state_not_stable_after_process_exit')
    if scope == 'reopen':
        seed = json.loads(helpers['read_owned'](Path(args.seed_record), 32768))
        if result['app_pid'] == seed['app_pid'] or data.get('seed_app_pid') != seed['app_pid']:
            raise ValueError('distinct_real_App_processes_required')
        if state != seed['state']:
            raise ValueError('final_saved_content_changed_after_strict_Undo')
    info = (Path(args.owned_state_root) / 'concat').lstat()
    return {'schema': 1, 'phase': scope, 'batch': args.state_token, 'root': args.owned_state_root,
            'app_pid': result['app_pid'], 'groups_gone': True,
            'app_sha256': WORKFLOW_APP_SHA, 'app_inode': [info.st_dev, info.st_ino],
            'strict_UI_export_pixel_check': True, 'state': state}


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "qa-script", "output", "expected-sha"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--client-binary")
    parser.add_argument("--candidate-manifest")
    parser.add_argument("--candidate-artifact-id")
    parser.add_argument("--source-head", help="Exact immutable App source HEAD; defaults to workflow HEAD")
    parser.add_argument("--identity-approval")
    parser.add_argument("--ui-approval")
    parser.add_argument('--owned-state-root', help='Only the two persistence scopes; fresh batch root for A, same original root for B')
    parser.add_argument('--state-token', help='32 lowercase random hex characters for this batch')
    parser.add_argument('--seed-record', help='B only; A launcher record written after its App/group exited')
    parser.add_argument("--input-dir", help="workflow-observation only; exact owned fixture inputs")
    parser.add_argument("--next-stage", choices=("observe-only", "navigation", "project-entry", "canvas-create-observation", "canvas-create-entry", "editor-entry-observation", "image-picker-observation", "workflow-observation", ALPHA_STAGE, "fresh-workbench-observation", *A2_SCOPES, *NATIVE_SCOPES, *PERSISTENCE_SCOPES), default="navigation")
    parser.add_argument("--isolated-display-capture", action="store_true",
                        help="Explicit isolated-display declaration for reviewed observation stages")
    parser.add_argument("--private-accessibility-bus", action="store_true",
                        help="workflow/fresh/A2 observation only; requires a direct dedicated dbus-run-session parent")
    parser.add_argument("--seconds", type=int, choices=(300, 420), default=300)
    args = parser.parse_args()
    capture_stages = {"image-picker-observation", "workflow-observation", ALPHA_STAGE, "fresh-workbench-observation", *A2_SCOPES, *NATIVE_SCOPES, *PERSISTENCE_SCOPES}
    if args.next_stage in capture_stages and not args.isolated_display_capture:
        parser.error(args.next_stage + " requires --isolated-display-capture")
    if args.isolated_display_capture and args.next_stage not in capture_stages:
        parser.error("--isolated-display-capture requires an authorized observation stage")
    if args.private_accessibility_bus and args.next_stage not in {"workflow-observation", ALPHA_STAGE, "fresh-workbench-observation", *A2_SCOPES, *NATIVE_SCOPES, *PERSISTENCE_SCOPES}:
        parser.error("--private-accessibility-bus is only valid for workflow/fresh observation")
    if args.input_dir is not None and args.next_stage not in ('workflow-observation', ALPHA_STAGE, 'assets-import', 'asset-library-flow', 'clip-media-import', *PERSISTENCE_SCOPES):
        parser.error('--input-dir is only valid for reviewed workflow, persistence or assets fixture scopes')
    if args.next_stage not in PERSISTENCE_SCOPES and any((args.owned_state_root, args.state_token, args.seed_record)):
        parser.error('persistence state options are forbidden for all original scopes')
    if args.next_stage in PERSISTENCE_SCOPES:
        if (not args.owned_state_root or not args.state_token or not re.fullmatch('[0-9a-f]{32}', args.state_token)
                or (args.next_stage == 'persistence-reopen-observation') != bool(args.seed_record)):
            parser.error('finite persistence stage requires its exact state/batch/seed arguments')
        expected_output_name = 'seed' if args.next_stage == 'persistence-seed-observation' else 'reopen'
        if Path(args.output).name != expected_output_name or Path(args.output).parent.parent != Path(args.owned_state_root).parent:
            parser.error('persistence scopes require sibling batch state and pair evidence paths')
        if args.seed_record and Path(args.seed_record) != Path(args.output).parent / 'seed' / WORKFLOW_DIRECTORY / 'persistence-seed.json':
            parser.error('B requires exactly this pair evidence root seed record')
    if args.next_stage == "workflow-observation" or args.next_stage in PERSISTENCE_SCOPES:
        if not args.private_accessibility_bus:
            parser.error("workflow-observation requires --private-accessibility-bus")
        if args.seconds != 300 or args.client_binary is not None:
            parser.error("workflow-observation requires 300 seconds and no MCP client")
        if not all((args.identity_approval, args.ui_approval, args.source_head,
                    args.candidate_manifest, args.candidate_artifact_id, args.input_dir)):
            parser.error("workflow-observation requires exact candidate provenance, declarations and owned fixtures")
        if args.source_head != WORKFLOW_HEAD or args.candidate_artifact_id != WORKFLOW_ARTIFACT:
            parser.error("workflow-observation requires the exact reviewed candidate source and artifact")
    if args.next_stage == ALPHA_STAGE:
        if (not args.private_accessibility_bus or args.seconds != 300 or args.client_binary is not None
                or not all((args.identity_approval, args.ui_approval, args.source_head,
                            args.candidate_manifest, args.candidate_artifact_id, args.input_dir))
                or args.source_head != ALPHA_HEAD or args.candidate_artifact_id != ALPHA_ARTIFACT):
            parser.error('alpha workflow requires its exact source, declarations, fixtures, private bus and300s')
    if args.next_stage == "fresh-workbench-observation":
        if not args.private_accessibility_bus:
            parser.error("fresh-workbench-observation requires --private-accessibility-bus")
        if args.seconds != 300 or args.client_binary is not None:
            parser.error("fresh-workbench-observation requires 300 seconds and no MCP client")
        if not all((args.identity_approval, args.ui_approval, args.candidate_manifest)):
            parser.error("fresh-workbench-observation requires candidate provenance and both declarations")
        if args.source_head != NEXT_UI_HEAD or args.candidate_artifact_id != NEXT_UI_ARTIFACT:
            parser.error("fresh-workbench-observation requires the exact reviewed candidate source and artifact")
    if args.next_stage in A2_SCOPES:
        if not args.private_accessibility_bus:
            parser.error("A2 observation requires --private-accessibility-bus")
        if args.seconds != 300 or args.client_binary is not None or args.input_dir is not None:
            parser.error("A2 observation requires 300 seconds, no MCP client and no fixture inputs")
        if not all((args.identity_approval, args.ui_approval, args.source_head, args.candidate_manifest)):
            parser.error("A2 observation requires candidate provenance and both declarations")
        if args.source_head != A2_HEAD or args.candidate_artifact_id != A2_ARTIFACT:
            parser.error("A2 observation requires the exact reviewed candidate source and artifact")
    if args.next_stage in NATIVE_SCOPES:
        if not args.private_accessibility_bus:
            parser.error("native observation requires --private-accessibility-bus")
        if args.seconds != 300 or args.client_binary is not None:
            parser.error('native observation requires 300 seconds and no MCP client')
        if (args.next_stage in ('assets-import', 'asset-library-flow', 'clip-media-import')) != (args.input_dir is not None):
            parser.error('only asset scopes require the exact single fixture input directory')
        if not all((args.identity_approval, args.ui_approval, args.source_head,
                    args.candidate_manifest, args.candidate_artifact_id)):
            parser.error("native observation requires exact candidate provenance and both declarations")
        native_head, _app_sha, _build_run, native_artifact, _identity_sha = native_runtime_identity(args.next_stage)
        if args.source_head != native_head or args.candidate_artifact_id != native_artifact:
            parser.error("native observation requires the exact reviewed candidate source and artifact")
    return args


def main():
    args = arguments()
    started = time.monotonic()
    deadline = started + args.seconds
    qa_deadline = deadline - 15
    processes, logs = [], []
    output = work = None
    phase = "setup"
    result = {"schema": 1, "status": "incomplete", "seconds": args.seconds,
              "next_stage": args.next_stage,
              "isolated_display_capture": args.isolated_display_capture,
              "private_accessibility_bus": args.private_accessibility_bus,
              "isolated_display_capture_source": ("explicit CLI option --isolated-display-capture"
                                                  if args.isolated_display_capture else None),
              "qa_contract": "one App lifetime; no detached children; known QA directory; secret-free artifacts",
              "product_acceptance": "not established by this harness"}
    exit_code = 1

    def interrupted(signum, _frame):
        if phase == "cleanup":
            return
        signal.setitimer(signal.ITIMER_REAL, 0)
        raise StopRun("signal " + str(signum))

    def hard_stop(_signum, _frame):
        for process in processes:
            kill_group(process, signal.SIGKILL)
        os._exit(124)  # Final wall limit; partial output/work may survive.

    previous = {sig: signal.signal(sig, interrupted)
                for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGALRM)}
    signal.setitimer(signal.ITIMER_REAL, max(0.001, qa_deadline - time.monotonic()))
    try:
        if platform.system() != "Linux":
            raise ValueError("normal App black-box runs require Linux")
        if not re.fullmatch(r"[0-9a-f]{40}", args.expected_sha):
            raise ValueError("--expected-sha must be 40 lowercase hex characters")
        if not os.environ.get("DISPLAY"):
            raise ValueError("DISPLAY is required")
        if args.private_accessibility_bus:
            verify_private_accessibility_bus(os.environ)
        if "SEECUT_UI_PREVIEW_DIR" in os.environ:
            raise ValueError("SEECUT_UI_PREVIEW_DIR must be absent")
        root = Path(__file__).resolve().parents[1]
        head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"],
                                       timeout=min(10, max(0.01, qa_deadline - time.monotonic())),
                                       stderr=subprocess.DEVNULL, text=True).strip()
        if head != args.expected_sha:
            raise ValueError("checkout HEAD does not match --expected-sha")
        source_head = args.source_head if args.source_head is not None else head
        if not re.fullmatch(r"[0-9a-f]{40}", source_head):
            raise ValueError("--source-head must be 40 lowercase hex characters")
        if args.source_head is not None and args.candidate_manifest is None:
            raise ValueError("explicit App source HEAD requires an immutable candidate manifest")
        if source_head != head and args.candidate_artifact_id is None:
            raise ValueError("cross-HEAD App source requires an exact candidate artifact ID")
        candidate = None
        if args.candidate_manifest is not None:
            if not Path(args.candidate_manifest).is_absolute():
                raise ValueError("--candidate-manifest must be absolute")
            source = regular_input(args.candidate_manifest)
            fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(fd, "rb") as stream:
                if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                    raise ValueError("candidate manifest changed to a non-regular file")
                raw = stream.read(APPROVAL_LIMIT + 1)
            if len(raw) > APPROVAL_LIMIT:
                raise ValueError("candidate manifest exceeds 16 KiB")
            candidate = json.loads(raw)
            fields = {"schema", "repository", "source_head", "platform", "app_sha256", "app_bytes",
                      "build_run_id", "build_job_id", "build_job", "build_job_name", "build_step_name"}
            if not isinstance(candidate, dict) or set(candidate) != fields:
                raise ValueError("invalid candidate manifest fields")
            if (type(candidate["schema"]) is not int or candidate["schema"] != 1
                    or candidate["repository"] != "Stormycry-cryp/seecut"
                    or candidate["source_head"] != source_head
                    or candidate["platform"] != "Linux x86_64" or platform.machine() != "x86_64"
                    or candidate["build_job"] != "engine"
                    or candidate["build_job_name"] != "Linux independent black-box QA"
                    or candidate["build_step_name"] != "Build the normal window candidate"
                    or not isinstance(candidate["app_sha256"], str)
                    or not re.fullmatch(r"[0-9a-f]{64}", candidate["app_sha256"])):
                raise ValueError("candidate provenance does not match the requested App source and platform")
            for key in ("app_bytes", "build_run_id", "build_job_id"):
                if type(candidate[key]) is not int or candidate[key] <= 0:
                    raise ValueError("candidate manifest requires positive integer sizes and IDs")
            if candidate["app_bytes"] > APP_LIMIT:
                raise ValueError("candidate App exceeds 512 MiB")
            result["candidate"] = dict(candidate,
                manifest_sha256=hashlib.sha256(raw).hexdigest(), source_artifact_id=None)
        if args.candidate_artifact_id is not None:
            if candidate is None or not re.fullmatch(r"[1-9][0-9]*", args.candidate_artifact_id):
                raise ValueError("candidate artifact ID requires a valid manifest")
            result["candidate"]["source_artifact_id"] = int(args.candidate_artifact_id)
        if args.next_stage == "fresh-workbench-observation":
            if (candidate is None or source_head != NEXT_UI_HEAD
                    or args.candidate_artifact_id != NEXT_UI_ARTIFACT
                    or candidate["app_sha256"] != NEXT_UI_APP_SHA
                    or candidate["build_run_id"] != NEXT_UI_BUILD_RUN):
                raise ValueError("fresh workbench candidate differs from its reviewed immutable provenance")
        if args.next_stage == "workflow-observation" or args.next_stage in A2_SCOPES or args.next_stage in PERSISTENCE_SCOPES:
            workflow = args.next_stage == "workflow-observation" or args.next_stage in PERSISTENCE_SCOPES
            reviewed_head, app_sha, build_run, artifact = (
                (WORKFLOW_HEAD, WORKFLOW_APP_SHA, WORKFLOW_BUILD_RUN, WORKFLOW_ARTIFACT) if workflow else
                (A2_HEAD, A2_APP_SHA, A2_BUILD_RUN, A2_ARTIFACT))
            if (candidate is None or source_head != reviewed_head or args.candidate_artifact_id != artifact
                    or candidate["app_sha256"] != app_sha or candidate["build_run_id"] != build_run):
                raise ValueError("reviewed candidate differs from its immutable provenance")
        if args.next_stage in NATIVE_SCOPES:
            native_head, native_app_sha, native_build_run, native_artifact, _identity_sha = native_runtime_identity(args.next_stage)
            if (candidate is None or source_head != native_head or args.candidate_artifact_id != native_artifact
                    or candidate["app_sha256"] != native_app_sha or candidate["build_run_id"] != native_build_run):
                raise ValueError("native candidate differs from its reviewed immutable provenance")
            if args.next_stage in ('clip-editor-entry', 'assistant-config-observation', 'asset-library-flow', 'clip-media-import') and (
                    candidate["build_job_id"] != 112019086814 or candidate["app_bytes"] != 467641344
                    or result["candidate"]["manifest_sha256"] != 'd666628e2499a26ef5f541898a4b5f0c8a69bba98bc6a3a28a2725d37c426d73'):
                raise ValueError("current candidate job, size or manifest differs from its immutable provenance")
        if args.next_stage == ALPHA_STAGE:
            if (candidate is None or source_head != ALPHA_HEAD or args.candidate_artifact_id != ALPHA_ARTIFACT
                    or candidate['app_sha256'] != ALPHA_APP_SHA or candidate['build_run_id'] != ALPHA_BUILD_RUN
                    or candidate['build_job_id'] != ALPHA_BUILD_JOB or candidate['app_bytes'] != ALPHA_APP_BYTES
                    or result['candidate']['manifest_sha256'] != ALPHA_MANIFEST_SHA):
                raise ValueError('alpha candidate differs from the immutable0aa source')
        sources = {"app": regular_input(args.binary), "qa": regular_input(args.qa_script)}
        if args.next_stage == ALPHA_STAGE:
            if sources['qa'] != root / 'scripts' / ALPHA_SCRIPTS['qa'][0]:
                raise ValueError('alpha workflow requires its fixed scope controller')
            for name, (filename, _digest) in ALPHA_SCRIPTS.items():
                if name != 'qa':
                    sources[name] = regular_input(root / 'scripts' / filename)
        if args.next_stage in NATIVE_SCOPES:
            scripts = NATIVE_SCOPES[args.next_stage]["scripts"]
            if args.next_stage == 'assistant-config-observation':
                destinations = ['independent-qa.py']
                for key, (filename, _digest) in scripts.items():
                    parts = Path(filename).parts
                    if len(parts) != 2 or parts[0] != 'assistant-0aa' or parts[1] in ('.', '..'):
                        raise ValueError('assistant source path outside its dedicated directory')
                    if key != 'qa':
                        destinations.append(parts[1])
                if len(set(destinations)) != len(destinations):
                    raise ValueError('assistant dependency destination basename collision')
            if sources["qa"] != root / "scripts" / scripts["qa"][0]:
                raise ValueError("native observation requires the fixed reviewed QA script")
            for name, (filename, _digest) in scripts.items():
                if name != "qa":
                    sources[name] = regular_input(root / "scripts" / filename)
        if args.next_stage in PERSISTENCE_SCOPES:
            scripts = PERSISTENCE_SCRIPTS[args.next_stage]
            if sources['qa'] != root / 'scripts' / scripts['qa'][0]:
                raise ValueError('persistence requires its fixed reviewed controller')
            for name, (filename, _digest) in scripts.items():
                if name != 'qa':
                    sources[name] = regular_input(root / 'scripts' / filename)
        if args.next_stage == "workflow-observation":
            if sources["qa"] != root / "scripts" / WORKFLOW_SCRIPTS["qa"][0]:
                raise ValueError("workflow-observation requires the fixed reviewed QA script")
            for name, (filename, _digest) in WORKFLOW_SCRIPTS.items():
                if name != "qa":
                    sources[name] = regular_input(root / "scripts" / filename)
        elif args.next_stage == "fresh-workbench-observation":
            if sources["qa"] != root / "scripts" / NEXT_UI_SCRIPT[0]:
                raise ValueError("fresh-workbench-observation requires the fixed reviewed QA script")
            sources["next_ui_probe"] = regular_input(root / "scripts" / NEXT_UI_PROBE[0])
        elif args.next_stage in A2_SCOPES:
            if sources["qa"] != root / "scripts" / A2_SCRIPTS["qa"][0]:
                raise ValueError("A2 observation requires the fixed reviewed QA script")
            for name, (filename, _digest) in A2_SCRIPTS.items():
                if name != "qa":
                    sources[name] = regular_input(root / "scripts" / filename)
        if args.client_binary is not None:
            if not Path(args.client_binary).is_absolute():
                raise ValueError("--client-binary must be absolute")
            sources["client"] = regular_input(args.client_binary)
        approval = None
        if args.identity_approval is not None:
            if not Path(args.identity_approval).is_absolute():
                raise ValueError("--identity-approval must be absolute")
            approval = regular_input(args.identity_approval)
            if approval.lstat().st_size > APPROVAL_LIMIT:
                raise ValueError("--identity-approval exceeds 16 KiB")
        ui_approval = None
        if args.ui_approval is not None:
            if not Path(args.ui_approval).is_absolute():
                raise ValueError("--ui-approval must be absolute")
            ui_approval = regular_input(args.ui_approval)
            if ui_approval.lstat().st_size > APPROVAL_LIMIT:
                raise ValueError("--ui-approval exceeds 16 KiB")
        output = fresh_output(args.output)
        work = Path(tempfile.mkdtemp(prefix="seecut-blackbox-"))
        copied = {"app": work / "concat", "client": work / "concat-editor-mcp",
                  "qa": work / "independent-qa.py"}
        if args.next_stage == ALPHA_STAGE:
            copied['qa'] = work / ALPHA_SCRIPTS['qa'][0]
            for name, (filename, _digest) in ALPHA_SCRIPTS.items():
                if name != 'qa':
                    copied[name] = work / filename
        if args.next_stage == "workflow-observation":
            for name, (filename, _digest) in WORKFLOW_SCRIPTS.items():
                if name != "qa":
                    copied[name] = work / filename
        elif args.next_stage == "fresh-workbench-observation":
            copied["next_ui_probe"] = work / NEXT_UI_PROBE[0]
        elif args.next_stage in A2_SCOPES:
            for name, (filename, _digest) in A2_SCRIPTS.items():
                if name != "qa":
                    copied[name] = work / filename
        if args.next_stage in NATIVE_SCOPES:
            for name, (filename, _digest) in NATIVE_SCOPES[args.next_stage]["scripts"].items():
                if name != "qa":
                    if args.next_stage == 'clip-media-import' and name == 'clip_import_controller':
                        # Shared clip-editor-action loads this fixed public interface basename.
                        copied[name] = work / 'clip-editor-controller.py'
                    else:
                        copied[name] = work / (Path(filename).name if args.next_stage == 'assistant-config-observation' else filename)
        if args.next_stage in PERSISTENCE_SCOPES:
            for name, (filename, _digest) in PERSISTENCE_SCRIPTS[args.next_stage].items():
                if name != 'qa':
                    copied[name] = work / ('persistence_state.py' if name == 'persistence_state' else filename)
            state = prepare_persistence_state(args, qa_deadline)
            copied['app'] = state / 'concat'
        hashes = {name: (hash_persistence_app(copied['app'], qa_deadline) if
                        args.next_stage == 'persistence-reopen-observation' and name == 'app' else
                        copy_and_hash(source, copied[name], executable=name in {"app", "client"},
                                      limit=(APP_LIMIT if name == "app" else
                                             (64 * 1024 if args.next_stage in ("workflow-observation", ALPHA_STAGE) or args.next_stage in A2_SCOPES or args.next_stage in NATIVE_SCOPES or args.next_stage in PERSISTENCE_SCOPES else None))))
                  for name, source in sources.items()}
        if args.next_stage == ALPHA_STAGE:
            for name, (_filename, digest) in ALPHA_SCRIPTS.items():
                if hashes[name] != digest:
                    raise ValueError('alpha scope script/helper SHA changed')
            if Path(args.input_dir) != root / 'scripts' / 'qa-fixtures':
                raise ValueError('alpha requires the fixed checked-out QA fixture directory')
            copied['input_dir'], fixture_hashes = copy_workflow_fixtures(Path(args.input_dir), work)
            result['input_fixture_sha256'] = fixture_hashes
        if args.next_stage in ('assets-import', 'asset-library-flow', 'clip-media-import'):
            if Path(args.input_dir) != root / 'scripts' / 'qa-fixtures':
                raise ValueError('asset scopes require the checked-out fixed fixture directory')
            copied['input_dir'], fixture_hashes = copy_assets_fixture(Path(args.input_dir), work)
            result['input_fixture_sha256'] = fixture_hashes
        if args.next_stage in PERSISTENCE_SCOPES:
            for name, (_filename, digest) in PERSISTENCE_SCRIPTS[args.next_stage].items():
                if hashes[name] != digest:
                    raise ValueError('persistence controller/helper raw identity changed')
            copied['input_dir'], fixture_hashes = copy_workflow_fixtures(Path(args.input_dir), work)
            result['input_fixture_sha256'] = fixture_hashes
        if args.next_stage == "workflow-observation":
            for name, (_filename, digest) in WORKFLOW_SCRIPTS.items():
                if hashes[name] != digest:
                    raise ValueError("workflow script differs from its reviewed SHA256")
            copied["input_dir"], fixture_hashes = copy_workflow_fixtures(Path(args.input_dir), work)
            result["input_fixture_sha256"] = fixture_hashes
        elif args.next_stage == "fresh-workbench-observation":
            if hashes["qa"] != NEXT_UI_SCRIPT[1]:
                raise ValueError("fresh workbench script differs from its reviewed SHA256")
            if hashes["next_ui_probe"] != NEXT_UI_PROBE[1]:
                raise ValueError("fresh workbench helper differs from its reviewed SHA256")
        elif args.next_stage in A2_SCOPES:
            for name, (_filename, digest) in A2_SCRIPTS.items():
                if hashes[name] != digest:
                    raise ValueError("A2 script or helper differs from its reviewed SHA256")
        if args.next_stage in NATIVE_SCOPES:
            for name, (_filename, digest) in NATIVE_SCOPES[args.next_stage]["scripts"].items():
                if hashes[name] != digest:
                    raise ValueError("native script or helper differs from its reviewed SHA256")
        if candidate is not None:
            if (hashes["app"] != candidate["app_sha256"]
                    or copied["app"].stat().st_size != candidate["app_bytes"]):
                raise ValueError("copied App does not match the immutable candidate manifest")
        if approval is not None:
            copied["identity_approval"] = work / "identity-approval.json"
            hashes["identity_approval"] = copy_and_hash(
                approval, copied["identity_approval"], limit=APPROVAL_LIMIT)
        if ui_approval is not None:
            copied["ui_approval"] = work / "ui-approval.json"
            hashes["ui_approval"] = copy_and_hash(
                ui_approval, copied["ui_approval"], limit=APPROVAL_LIMIT)
        if args.next_stage == "workflow-observation" or args.next_stage in A2_SCOPES or args.next_stage in PERSISTENCE_SCOPES:
            workflow = args.next_stage == "workflow-observation" or args.next_stage in PERSISTENCE_SCOPES
            identity_sha, ui_sha, reviewed_head, app_sha = (
                (WORKFLOW_IDENTITY_SHA, WORKFLOW_UI[1], WORKFLOW_HEAD, WORKFLOW_APP_SHA) if workflow else
                (A2_IDENTITY_SHA, A2_UI[1], A2_HEAD, A2_APP_SHA))
            if hashes.get("identity_approval") != identity_sha:
                raise ValueError("runtime identity differs from its reviewed raw bytes")
            if hashes.get("ui_approval") != ui_sha:
                raise ValueError("UI declaration differs from its reviewed raw bytes")
            identity = json.loads(copied["identity_approval"].read_bytes())
            if (not isinstance(identity, dict) or identity.get("schema") != 2
                    or identity.get("reviewed_by") != "main-reviewer" or identity.get("runtime_head") != reviewed_head
                    or identity.get("runtime_app_sha256") != app_sha or identity.get("change_scope") != "product-candidate"):
                raise ValueError("main runtime identity differs from the reviewed App")
        if args.next_stage in NATIVE_SCOPES:
            native_head, native_app_sha, _build_run, _artifact, native_identity_sha = native_runtime_identity(args.next_stage)
            if hashes.get("identity_approval") != native_identity_sha:
                raise ValueError("native runtime identity differs from its reviewed raw bytes")
            if hashes.get("ui_approval") != NATIVE_SCOPES[args.next_stage]["ui"][1]:
                raise ValueError("native UI declaration differs from its reviewed raw bytes")
            identity = json.loads(copied["identity_approval"].read_bytes())
            if (not isinstance(identity, dict) or identity.get("schema") != 2
                    or identity.get("reviewed_by") != "main-reviewer" or identity.get("runtime_head") != native_head
                    or identity.get("runtime_app_sha256") != native_app_sha or identity.get("change_scope") != "product-candidate"):
                raise ValueError("main native runtime identity differs from the reviewed App")
        if args.next_stage == ALPHA_STAGE:
            if hashes.get('identity_approval') != ALPHA_IDENTITY_SHA or hashes.get('ui_approval') != ALPHA_UI_SHA:
                raise ValueError('alpha declaration raw identity changed')
            identity = json.loads(copied['identity_approval'].read_bytes())
            if (identity.get('schema') != 2 or identity.get('reviewed_by') != 'main-reviewer'
                    or identity.get('runtime_head') != ALPHA_HEAD or identity.get('runtime_app_sha256') != ALPHA_APP_SHA
                    or identity.get('change_scope') != 'product-candidate'):
                raise ValueError('alpha runtime declaration differs from0aa App')
        portable = (Path(args.owned_state_root) if args.next_stage in PERSISTENCE_SCOPES else work) / 'portable'
        prefs = {"locale": "en", "dark": False, "server": {"enabled": False}}
        if args.next_stage != 'persistence-reopen-observation':
            portable.mkdir(mode=0o700)
            (portable / 'settings.json').write_text(json.dumps(prefs), encoding='utf-8')
        if args.next_stage in PERSISTENCE_SCOPES:
            result['persistence'] = {'batch': args.state_token, 'root': args.owned_state_root,
                                     'phase': args.next_stage, 'original_path_reused': True}
            if args.next_stage == 'persistence-reopen-observation':
                result['persistence']['prelaunch_state'] = persistence_prelaunch(args, qa_deadline)
                (output / WORKFLOW_DIRECTORY).mkdir(mode=0o700)
                write_owned(output / WORKFLOW_DIRECTORY, 'persistence-prelaunch.json',
                            (json.dumps(result['persistence']['prelaunch_state'], separators=(',', ':')) + '\n').encode())
        env = os.environ.copy()
        if args.private_accessibility_bus:
            for key in ("AT_SPI_BUS_ADDRESS", "DBUS_STARTER_ADDRESS", "DBUS_STARTER_BUS_TYPE"):
                env.pop(key, None)
        env["SLINT_SCALE_FACTOR"] = "1"
        for key in ("TMPDIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"):
            isolated = work / key.lower()
            isolated.mkdir(mode=0o700)
            env[key] = str(isolated)
        for key in ("SEECUT_MCP_CLIENT_TOKEN", "SEECUT_MCP_WRITE_TOKEN"):
            env.pop(key, None)
        result.update(head=head, workflow_head=head, source_head=source_head,
                      expected_head=args.expected_sha, sha256=hashes,
                      app_binary=str(copied["app"]),
                      platform=platform.platform(), display=env["DISPLAY"],
                      slint_scale_factor=env["SLINT_SCALE_FACTOR"],
                      cpu={"logical_count": os.cpu_count(), "machine": platform.machine(),
                           "slint_wgpu_cpu": env.get("SLINT_WGPU_CPU") == "1"},
                      vulkan_icd=("[redacted]" if SENSITIVE.search(env.get("VK_ICD_FILENAMES", "").encode())
                                  else env.get("VK_ICD_FILENAMES", "")),
                      startup_preferences=prefs,
                      preference_source="explicit isolated configuration; not UI observation",
                      isolation="copied executable beside fresh portable; preserved HOME; private TMPDIR/XDG",
                      limits={"artifact_total_bytes": ARTIFACT_LIMIT,
                              "wall_seconds": args.seconds, "cleanup_reserve_seconds": 15,
                              "log_bytes_per_process": LOG_LIMIT})
        if args.next_stage in NATIVE_SCOPES:
            result["limits"]["controller_seconds"] = NATIVE_CONTROLLER_SECONDS
        if args.next_stage in A2_SCOPES:
            result["limits"]["controller_seconds"] = A2_CONTROLLER_SECONDS
        if args.next_stage in {"workflow-observation", ALPHA_STAGE, "fresh-workbench-observation", *A2_SCOPES, *NATIVE_SCOPES, *PERSISTENCE_SCOPES}:
            phase = "accessibility_preparation"
            enable_private_accessibility(env, qa_deadline, result)
        phase = "launch"
        app = spawn([str(copied["app"])], work, env)
        processes.append(app)
        logs.append(BoundedLog(app.stdout))
        result["app_pid"] = app.pid
        phase = "visible_window_readiness"
        result["app_window_id"] = wait_for_window(app, work, env, qa_deadline)
        result["client_binary_available"] = args.client_binary is not None
        command = qa_command(args, copied, app.pid, result["app_window_id"],
                             work, output, source_head, qa_deadline)
        qa = spawn(command, work, env)
        processes.append(qa)
        logs.append(BoundedLog(qa.stdout))
        result["qa_pid"] = qa.pid
        phase = "qa"
        while qa.poll() is None:
            inspect_artifacts(output, ARTIFACT_LIMIT - HARNESS_RESERVE, args.next_stage)
            if time.monotonic() >= qa_deadline:
                interrupted(signal.SIGALRM, None)
            time.sleep(min(0.25, max(0, qa_deadline - time.monotonic())))
        result["qa_exit_code"] = qa.returncode
        result["status"] = {0: "qa_exited", 3: "qa_review_required"}.get(qa.returncode, "qa_failed")
        exit_code = qa.returncode if qa.returncode in (0, 3) else 1
    except StopRun:
        result["status"] = "interrupted_or_timed_out"
        exit_code = 124
    except (OSError, ValueError, subprocess.SubprocessError):
        result["status"] = "harness_error"
        result["error_phase"] = phase  # Never dump arbitrary stderr/exception secrets.
    finally:
        phase = "cleanup"
        signal.setitimer(signal.ITIMER_REAL, 0)
        # Ignore repeated cancellation during bounded cleanup; ALRM still kills.
        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(sig, signal.SIG_IGN)
        signal.signal(signal.SIGALRM, hard_stop)
        signal.setitimer(signal.ITIMER_REAL, max(0.001, deadline - time.monotonic()))
        try:
            stop_processes(processes, deadline)
            for log in logs:
                log.thread.join(timeout=min(0.5, max(0, deadline - time.monotonic())))
            if args.next_stage in PERSISTENCE_SCOPES and output is not None and result.get('qa_exit_code') == 0:
                try:
                    record = persistence_after_exit(args, result, processes, output, deadline)
                    record_name = 'persistence-seed.json' if args.next_stage == 'persistence-seed-observation' else 'persistence-postrun.json'
                    write_owned(output / WORKFLOW_DIRECTORY, record_name,
                                (json.dumps(record, separators=(',', ':')) + '\n').encode())
                    result['persistence']['all_owned_process_groups_gone'] = True
                except Exception as error:
                    # Verification failure must not skip work removal or harness/log output.
                    detail = str(error)
                    reason = detail if re.fullmatch('[A-Za-z0-9_]{1,96}', detail) else 'state_verification_io_or_parse_failed'
                    result['status'] = 'persistence_state_failed'
                    result.setdefault('persistence', {})['failure_reason'] = reason
                    exit_code = 1
                    try:
                        failure = {'schema': 1, 'phase': args.next_stage,
                                   'batch': args.state_token, 'reason': reason}
                        if reason == 'unexpected_portable_state_entry':
                            try:
                                failure['portable_entries'] = persistence_helpers()['portable_entry_diagnostic'](
                                    Path(args.owned_state_root), args.state_token, deadline)
                            except (OSError, ValueError, TypeError):
                                failure['portable_entries_unavailable'] = True
                        write_owned(output / WORKFLOW_DIRECTORY, 'persistence-failure.json',
                                    (json.dumps(failure, separators=(',', ':')) + '\n').encode())
                    except OSError:
                        result['persistence']['failure_record_unavailable'] = True
            result["process_exit_codes"] = [process.returncode for process in processes]
            result["log_truncated"] = [log.truncated for log in logs]
            for process in processes:
                if process.stdout is not None and not logs:
                    process.stdout.close()
            if work is not None:
                shutil.rmtree(work)  # Only this invocation's mkdtemp directory.
            result["owned_work_removed"] = work is None or not work.exists()
            if output is not None:
                qa_bytes = inspect_artifacts(output, next_stage=args.next_stage)
                if any((output / name).exists() or (output / name).is_symlink() for name in RESERVED):
                    raise ValueError("QA used a reserved harness output name")
                payloads = {name: log.snapshot() for name, log in zip(("app.log", "qa.log"), logs)}
                result["elapsed_seconds"] = round(time.monotonic() - started, 3)
                payloads["harness.json"] = (json.dumps(result, indent=2) + "\n").encode()
                if qa_bytes + sum(map(len, payloads.values())) > ARTIFACT_LIMIT:
                    raise ValueError("combined output exceeds 15 MiB")
                for name, payload in payloads.items():
                    write_owned(output, name, payload)
                inspect_artifacts(output, next_stage=args.next_stage)
        except (OSError, ValueError, subprocess.SubprocessError):
            result["status"] = "cleanup_or_artifact_error"
            exit_code = 1
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            for sig, handler in previous.items():
                signal.signal(sig, handler)
    print(json.dumps({"status": result["status"], "product_acceptance": result["product_acceptance"]}))
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
