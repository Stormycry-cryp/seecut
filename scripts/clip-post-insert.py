#!/usr/bin/env python3
"""Owned clip move/undo/save and File observation. No close/open/export input."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess

# Replaced only by prepare.py from main-reviewed v4 public metadata and PNG.
COLUMNS = ('action_interface', 'allowed_actions', 'bounds', 'button', 'checked', 'dialog', 'editable', 'editable_text_interface', 'enabled', 'entry', 'file_chooser', 'focusable', 'focused', 'label', 'modal', 'panel', 'path', 'radio', 'role', 'selected', 'sensitive', 'showing')
BASE_ROWS = [[False, {'absent': True}, {'absent': True}, False, False, False, False, False, False, False, False, False, False, None, False, False, [], False, 75, False, False, False], [False, {'absent': True}, {'x': 0, 'y': 0, 'width': 1280, 'height': 900}, False, False, False, False, False, True, False, False, True, True, None, False, False, [0], False, 23, False, True, True], [False, {'absent': True}, {'x': 14, 'y': 9, 'width': 44, 'height': 13}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 0], False, 29, False, True, True], [False, {'absent': True}, {'x': 74, 'y': 10, 'width': 19, 'height': 12}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 1], False, 29, False, True, True], [False, {'absent': True}, {'x': 111, 'y': 10, 'width': 21, 'height': 12}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 2], False, 29, False, True, True], [False, {'absent': True}, {'x': 150, 'y': 10, 'width': 26, 'height': 12}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 3], False, 29, False, True, True], [False, {'absent': True}, {'x': 18, 'y': 90, 'width': 44, 'height': 44}, True, False, False, False, False, True, False, False, True, False, '生成', False, False, [0, 4], False, 43, False, True, True], [False, {'absent': True}, {'x': 18, 'y': 148, 'width': 44, 'height': 44}, True, False, False, False, False, True, False, False, True, False, '画布', False, False, [0, 5], False, 43, False, True, True], [False, {'absent': True}, {'x': 18, 'y': 206, 'width': 44, 'height': 44}, True, False, False, False, False, True, False, False, True, False, '剪辑', False, False, [0, 6], False, 43, False, True, True], [False, {'absent': True}, {'x': 18, 'y': 264, 'width': 44, 'height': 44}, True, False, False, False, False, True, False, False, True, False, '资产库', False, False, [0, 7], False, 43, False, True, True], [True, {'absent': True}, {'x': 18, 'y': 726, 'width': 44, 'height': 44}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 8], False, 62, False, True, True], [True, ['click'], {'x': 18, 'y': 784, 'width': 44, 'height': 44}, True, False, False, False, False, True, False, False, True, False, '设置', False, False, [0, 9], False, 43, False, True, True], [False, {'absent': True}, {'x': 18, 'y': 842, 'width': 44, 'height': 44}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 10], False, 43, False, True, True], [False, {'absent': True}, {'x': 22, 'y': 846, 'width': 36, 'height': 36}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 10, 0], False, 27, False, True, True], [False, {'absent': True}, {'x': 100, 'y': 42, 'width': 956, 'height': 34}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 11], False, 29, False, True, True], [True, ['click'], {'x': 1064, 'y': 42, 'width': 108, 'height': 40}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 12], False, 43, False, True, True], [False, {'absent': True}, {'x': 1076, 'y': 42, 'width': 84, 'height': 40}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 12, 0], False, 29, False, True, True], [True, ['click'], {'x': 1180, 'y': 42, 'width': 80, 'height': 40}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 13], False, 43, False, True, True], [False, {'absent': True}, {'x': 1192, 'y': 42, 'width': 56, 'height': 40}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 13, 0], False, 29, False, True, True], [True, {'absent': True}, {'x': 98, 'y': 100, 'width': 47, 'height': 32}, False, False, False, False, False, True, False, False, True, False, 'Media', False, False, [0, 14], False, 37, True, True, True], [True, {'absent': True}, {'x': 147, 'y': 100, 'width': 47, 'height': 32}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 15], False, 37, False, True, True], [True, {'absent': True}, {'x': 197, 'y': 100, 'width': 47, 'height': 32}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 16], False, 37, False, True, True], [True, {'absent': True}, {'x': 247, 'y': 100, 'width': 47, 'height': 32}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 17], False, 37, False, True, True], [True, {'absent': True}, {'x': 297, 'y': 100, 'width': 47, 'height': 32}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 18], False, 37, False, True, True], [True, {'absent': True}, {'x': 347, 'y': 100, 'width': 47, 'height': 32}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 19], False, 37, False, True, True], [False, {'absent': True}, {'x': 112, 'y': 142, 'width': 70, 'height': 26}, False, False, False, False, False, True, False, False, False, False, 'Media', False, False, [0, 20], False, 29, False, True, True], [False, {'absent': True}, {'x': 110, 'y': 169, 'width': 62, 'height': 24}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 21], False, 29, False, True, True], [False, {'absent': True}, {'x': 176, 'y': 169, 'width': 6, 'height': 24}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 22], False, 29, False, True, True], [False, {'absent': True}, {'x': 110, 'y': 194, 'width': 72, 'height': 24}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 23], False, 29, False, True, True], [False, {'absent': True}, {'x': 110, 'y': 219, 'width': 72, 'height': 24}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 24], False, 29, False, True, True], [False, {'absent': True}, {'x': 110, 'y': 244, 'width': 62, 'height': 24}, False, False, False, False, False, True, False, False, False, False, 'Images', False, False, [0, 25], False, 29, False, True, True], [False, {'absent': True}, {'x': 176, 'y': 244, 'width': 6, 'height': 24}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 26], False, 29, False, True, True], [False, {'absent': True}, {'x': 241, 'y': 146, 'width': 36, 'height': 28}, False, False, False, False, False, True, False, False, False, False, 'Import', False, False, [0, 27], False, 29, False, True, True], [False, {'absent': True}, {'x': 201, 'y': 182, 'width': 117, 'height': 66}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 28], False, 27, False, True, True], [False, {'absent': True}, {'x': 293, 'y': 233, 'width': 18, 'height': 10}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 29], False, 29, False, True, True], [False, {'absent': True}, {'x': 201, 'y': 252, 'width': 117, 'height': 14}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 30], False, 29, False, True, True], [False, {'absent': True}, {'x': 474, 'y': 94, 'width': 48, 'height': 44}, False, False, False, False, False, True, False, False, False, False, 'Preview', False, False, [0, 31], False, 29, False, True, True], [False, {'absent': True}, {'x': 693, 'y': 110, 'width': 265, 'height': 12}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 32], False, 29, False, True, True], [False, {'absent': True}, {'x': 476, 'y': 185, 'width': 515, 'height': 289}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 33], False, 27, False, True, True], [False, {'absent': True}, {'x': 709, 'y': 482, 'width': 50, 'height': 18}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 34], False, 29, False, True, True], [False, {'absent': True}, {'x': 478, 'y': 531, 'width': 104, 'height': 28}, False, False, False, True, False, True, True, False, True, False, None, False, False, [0, 35], False, 79, False, True, True], [False, {'absent': True}, {'x': 594, 'y': 522, 'width': 71, 'height': 46}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 36], False, 29, False, True, True], [True, ['click'], {'x': 693, 'y': 522, 'width': 26, 'height': 46}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 37], False, 43, False, True, True], [True, ['click'], {'x': 721, 'y': 522, 'width': 26, 'height': 46}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 38], False, 43, False, True, True], [True, ['click'], {'x': 749, 'y': 522, 'width': 26, 'height': 46}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 39], False, 43, False, True, True], [True, ['click'], {'x': 777, 'y': 522, 'width': 26, 'height': 46}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 40], False, 43, False, True, True], [True, ['click'], {'x': 805, 'y': 522, 'width': 26, 'height': 46}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 41], False, 43, False, True, True], [False, {'absent': True}, {'x': 919, 'y': 531, 'width': 22, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 42], False, 29, False, True, True], [True, ['click'], {'x': 971, 'y': 532, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 43], False, 43, False, True, True], [True, {'absent': True}, {'x': 1026, 'y': 105, 'width': 93, 'height': 22}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 44], False, 37, True, True, True], [False, {'absent': True}, {'x': 1026, 'y': 105, 'width': 93, 'height': 22}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 44, 0], False, 29, False, True, True], [True, {'absent': True}, {'x': 1120, 'y': 105, 'width': 93, 'height': 22}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 45], False, 37, False, True, True], [False, {'absent': True}, {'x': 1120, 'y': 105, 'width': 93, 'height': 22}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 45, 0], False, 29, False, True, True], [True, {'absent': True}, {'x': 1026, 'y': 147, 'width': 78, 'height': 22}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 46], False, 37, True, True, True], [False, {'absent': True}, {'x': 1026, 'y': 147, 'width': 78, 'height': 22}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 46, 0], False, 29, False, True, True], [True, {'absent': True}, {'x': 1104, 'y': 147, 'width': 78, 'height': 22}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 47], False, 37, False, True, True], [False, {'absent': True}, {'x': 1104, 'y': 147, 'width': 78, 'height': 22}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 47, 0], False, 29, False, True, True], [True, {'absent': True}, {'x': 1182, 'y': 147, 'width': 78, 'height': 22}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 48], False, 37, False, True, True], [False, {'absent': True}, {'x': 1182, 'y': 147, 'width': 78, 'height': 22}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 48, 0], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 178, 'width': 61, 'height': 36}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 49], False, 29, False, True, True], [False, {'absent': True}, {'x': 1247, 'y': 191, 'width': 6, 'height': 10}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 50], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 218, 'width': 30, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 51], False, 29, False, True, True], [False, {'absent': True}, {'x': 1123, 'y': 218, 'width': 46, 'height': 28}, False, False, False, True, False, True, True, False, True, False, None, False, False, [0, 52], False, 79, False, True, True], [False, {'absent': True}, {'x': 1174, 'y': 218, 'width': 25, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 53], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 252, 'width': 54, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 54], False, 29, False, True, True], [False, {'absent': True}, {'x': 1123, 'y': 252, 'width': 46, 'height': 28}, False, False, False, True, False, True, True, False, True, False, None, False, False, [0, 55], False, 79, False, True, True], [False, {'absent': True}, {'x': 1174, 'y': 252, 'width': 25, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 56], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 286, 'width': 55, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 57], False, 29, False, True, True], [False, {'absent': True}, {'x': 1123, 'y': 286, 'width': 46, 'height': 28}, False, False, False, True, False, True, True, False, True, False, None, False, False, [0, 58], False, 79, False, True, True], [False, {'absent': True}, {'x': 1174, 'y': 286, 'width': 25, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 59], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 320, 'width': 46, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 60], False, 29, False, True, True], [False, {'absent': True}, {'x': 1123, 'y': 320, 'width': 52, 'height': 28}, False, False, False, True, False, True, True, False, True, False, None, False, False, [0, 61], False, 79, False, True, True], [False, {'absent': True}, {'x': 1180, 'y': 320, 'width': 19, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 62], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 354, 'width': 32, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 63], False, 29, False, True, True], [False, {'absent': True}, {'x': 1182, 'y': 354, 'width': 46, 'height': 28}, False, False, False, True, False, True, True, False, True, False, None, False, False, [0, 64], False, 79, False, True, True], [False, {'absent': True}, {'x': 1233, 'y': 354, 'width': 25, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 65], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 388, 'width': 36, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 66], False, 29, False, True, True], [False, {'absent': True}, {'x': 1182, 'y': 388, 'width': 46, 'height': 28}, False, False, False, True, False, True, True, False, True, False, None, False, False, [0, 67], False, 79, False, True, True], [False, {'absent': True}, {'x': 1233, 'y': 388, 'width': 25, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 68], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 429, 'width': 35, 'height': 36}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 69], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 469, 'width': 42, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 70], False, 29, False, True, True], [False, {'absent': True}, {'x': 1123, 'y': 469, 'width': 46, 'height': 28}, False, False, False, True, False, True, True, False, True, False, None, False, False, [0, 71], False, 79, False, True, True], [False, {'absent': True}, {'x': 1174, 'y': 469, 'width': 25, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 72], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 503, 'width': 62, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 73], False, 29, False, True, True], [False, {'absent': True}, {'x': 1107, 'y': 503, 'width': 126, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 74], False, 29, False, True, True], [False, {'absent': True}, {'x': 1029, 'y': 537, 'width': 76, 'height': 28}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 75], False, 29, False, True, True], [True, {'absent': True}, {'x': 1228, 'y': 542, 'width': 30, 'height': 17}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 76], False, 7, False, True, True], [False, {'absent': True}, {'x': 106, 'y': 588, 'width': 55, 'height': 12}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 77], False, 29, False, True, True], [True, ['click'], {'x': 96, 'y': 616, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 78], False, 43, False, True, True], [True, {'absent': True}, {'x': 139, 'y': 616, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 79], False, 62, False, True, True], [True, {'absent': True}, {'x': 169, 'y': 616, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 80], False, 62, False, True, True], [True, ['click'], {'x': 212, 'y': 616, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 81], False, 43, False, True, True], [True, ['click'], {'x': 242, 'y': 616, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 82], False, 43, False, True, True], [True, ['click'], {'x': 272, 'y': 616, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 83], False, 43, False, True, True], [True, {'absent': True}, {'x': 315, 'y': 616, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 84], False, 62, False, True, True], [True, {'absent': True}, {'x': 358, 'y': 616, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 85], False, 62, False, True, True], [True, ['click'], {'x': 401, 'y': 616, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 86], False, 43, False, True, True], [True, ['click'], {'x': 431, 'y': 616, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 87], False, 43, False, True, True], [False, {'absent': True}, {'x': 1069, 'y': 616, 'width': 92, 'height': 26}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 88], False, 29, False, True, True], [True, ['click'], {'x': 1178, 'y': 616, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 89], False, 43, False, True, True], [True, ['click'], {'x': 1208, 'y': 616, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 90], False, 43, False, True, True], [True, ['click'], {'x': 1238, 'y': 616, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 91], False, 43, False, True, True], [True, ['click'], {'x': 133, 'y': 683, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 92], False, 43, False, True, True], [True, {'absent': True}, {'x': 160, 'y': 683, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 93], False, 62, False, True, True], [True, {'absent': True}, {'x': 187, 'y': 683, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 94], False, 62, False, True, True], [True, {'absent': True}, {'x': 214, 'y': 683, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 95], False, 62, False, True, True], [True, ['click'], {'x': 133, 'y': 743, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 96], False, 43, False, True, True], [True, {'absent': True}, {'x': 160, 'y': 743, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 97], False, 62, False, True, True], [True, {'absent': True}, {'x': 187, 'y': 743, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 98], False, 62, False, True, True], [True, {'absent': True}, {'x': 214, 'y': 743, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 99], False, 62, False, True, True], [True, ['click'], {'x': 133, 'y': 803, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 100], False, 43, False, True, True], [True, {'absent': True}, {'x': 160, 'y': 803, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 101], False, 62, False, True, True], [True, {'absent': True}, {'x': 187, 'y': 803, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 102], False, 62, False, True, True], [True, {'absent': True}, {'x': 214, 'y': 803, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 103], False, 62, False, True, True], [True, ['click'], {'x': 133, 'y': 863, 'width': 26, 'height': 26}, True, False, False, False, False, True, False, False, True, False, None, False, False, [0, 104], False, 43, False, True, True], [True, {'absent': True}, {'x': 160, 'y': 863, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 105], False, 62, False, True, True], [True, {'absent': True}, {'x': 187, 'y': 863, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 106], False, 62, False, True, True], [True, {'absent': True}, {'x': 214, 'y': 863, 'width': 26, 'height': 26}, False, False, False, False, False, True, False, False, True, False, None, False, False, [0, 107], False, 62, False, True, True], [False, {'absent': True}, {'x': 245, 'y': 878, 'width': 70, 'height': 53}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 108], False, 27, False, True, True], [False, {'absent': True}, {'x': 315, 'y': 878, 'width': 70, 'height': 53}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 109], False, 27, False, True, True], [False, {'absent': True}, {'x': 252, 'y': 861, 'width': 86, 'height': 17}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 110], False, 29, False, True, True], [False, {'absent': True}, {'x': 250, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 111], False, 29, False, True, True], [False, {'absent': True}, {'x': 350, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 112], False, 29, False, True, True], [False, {'absent': True}, {'x': 450, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 113], False, 29, False, True, True], [False, {'absent': True}, {'x': 550, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 114], False, 29, False, True, True], [False, {'absent': True}, {'x': 650, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 115], False, 29, False, True, True], [False, {'absent': True}, {'x': 750, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 116], False, 29, False, True, True], [False, {'absent': True}, {'x': 850, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 117], False, 29, False, True, True], [False, {'absent': True}, {'x': 950, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 118], False, 29, False, True, True], [False, {'absent': True}, {'x': 1050, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 119], False, 29, False, True, True], [False, {'absent': True}, {'x': 1150, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 120], False, 29, False, True, True], [False, {'absent': True}, {'x': 1250, 'y': 649, 'width': 57, 'height': 21}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 121], False, 29, False, True, True], [False, {'absent': True}, {'x': 1174, 'y': 864, 'width': 78, 'height': 32}, False, False, False, False, False, True, False, False, False, False, None, False, False, [0, 122], False, 29, False, True, True]]
GUARDS = {'select-tool': {'region': [139, 616, 26, 26], 'rgb_sha256': '854d3221f53b108129e894d91810138a371237ee00a774aa67296d59f383f0ae'}, 'unlocked-track': {'region': [160, 863, 26, 26], 'rgb_sha256': '396a9ef60b9033e01e8d4a1f19aafe2837f9cd51dd891adce2176010c1a076f9'}, 'ruler': {'region': [245, 649, 200, 21], 'rgb_sha256': '5308bb802e6a20cac061846600e9122ba07e6d1c2c1f0e68084e1122d1b1a88c'}, 'file-label': {'region': [74, 10, 19, 12], 'rgb_sha256': '1fc53c50b5dec07c703c5307387689f79ffe8ce0a88e73fca0dbb00ededd585e'}, 'clip-inner': {'region': [247, 861, 96, 31], 'rgb_sha256': '945dfdf207d2eed1318a604259b84e1ca3e32ca865f36e0d0c7b595e8155d645'}, 'preview': {'region': [476, 185, 515, 289], 'rgb_sha256': 'a4ce6891fd6e8e80dfa1220fef983d3b76567ff5c03cdc6bf4ea11081eeba7a8'}, 'moved-gap': {'region': [247, 878, 16, 12], 'rgb_sha256': 'f2f977bc947f6128081fe2eb6b0c0053503271493e6f497b771385b5ed10868a'}}
PROBE_SHA = '562612f3bf1d54da2f9011fdea0849b6ecdf0dc66d129b42546fb0bd21bc3cd8'
CAPTIONS = frozenset({'File', 'Open project…', 'Close project'})


def extended_probe():
    path = Path(__file__).with_name('clip-editor-public-probe.py')
    if (path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid()
            or path.stat().st_size > 65536 or hashlib.sha256(path.read_bytes()).hexdigest() != PROBE_SHA):
        raise ValueError('fixed_same_owned_nonfield_probe_required')
    spec = importlib.util.spec_from_file_location('post_insert_public_probe', path)
    probe = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(probe)
    # Its original no_name/file-list/interface exclusions execute unchanged.
    probe.SAFE_LABELS = probe.SAFE_LABELS | CAPTIONS
    return probe


def base_nodes():
    return [{key: value for key, value in zip(COLUMNS, row) if value != {'absent': True}}
            for row in BASE_ROWS]


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'))


def signature(node):
    return canonical({key: value for key, value in node.items() if key != 'path'})


def interactive(node):
    return any(node[key] for key in ('action_interface', 'focusable', 'entry', 'editable', 'editable_text_interface'))


def inert_success_toast(nodes, resized=False):
    """Source success Text; a failure's Copy chip cannot match this terminal node."""
    node, original = nodes[-1], base_nodes()[-1]
    bounds = node.get('bounds')
    if (bounds is None or not 1 <= bounds['width'] <= 375 or bounds['height'] != 32
            or not 852 <= bounds['y'] <= 864 or bounds['x'] + bounds['width'] != 1252):
        return False
    if not resized and (bounds['x'] != 1174 or bounds['width'] != 78):
        return False
    return {k: v for k, v in node.items() if k not in ('path', 'bounds')} == {
        k: v for k, v in original.items() if k not in ('path', 'bounds')}


def baseline_matches(nodes, rows):
    current = rows(nodes)
    if len(current) != 133 or not inert_success_toast(nodes):
        return False
    adjusted = list(current)
    adjusted[-1] = list(current[-1])
    adjusted[-1][COLUMNS.index('bounds')] = dict(nodes[-1]['bounds'], y=864)
    return adjusted == BASE_ROWS


def semantic_clip_matches(nodes, delta, saved=False):
    """Observed single clip geometry; other interactive controls stay exact.

    Readonly preview/text layout may change at playhead 0 after a right move.
    Paths may shift as the preview unloads. No new interactive widget, focus,
    timeline child, selection/tool/track control or error Copy is accepted.
    """
    original = base_nodes()
    if sorted(signature(n) for n in nodes if interactive(n)) != sorted(
            signature(n) for n in original if interactive(n)):
        return False
    if not inert_success_toast(nodes, resized=saved):
        return False
    def clip_children(group):
        return [n for n in group[:-1] if n['role'] in (27, 29) and n.get('bounds', {}).get('x', -1) >= 245
                and n.get('bounds', {}).get('y', -1) >= 676]
    expected = clip_children(original)
    translated = []
    for node in expected:
        item = dict(node)
        item['bounds'] = dict(node['bounds'], x=node['bounds']['x'] + delta)
        translated.append(signature(item))
    if len(expected) != 3 or sorted(signature(n) for n in clip_children(nodes)) != sorted(translated):
        return False
    # Clip workspace navigation, title-bar File and actual 0/5-second ruler.
    anchors = [n for n in original if n['path'] in ([0, 1], [0, 6], [0, 111], [0, 112])]
    return len(anchors) == 4 and all(sum(signature(n) == signature(a) for n in nodes) == 1 for a in anchors)


def run(*, context, rows, stop, probe, pixels, guard, focus, command, pause, snapshot,
        report, args, remaining, fixture, manifest, release=None):
    def checked(name, allow_dialog=False):
        data = probe(name, allow_dialog=allow_dialog)
        context(data, args.app_pid, allow_dialog=allow_dialog)
        rows(data['nodes'])
        if not allow_dialog and any(n['focused'] and n['role'] != 23 for n in data['nodes']):
            raise stop('post_insert_nonwindow_focus_no_input')
        return data

    def frame_guards(frame, delta=0, preview=False):
        for name in ('select-tool', 'unlocked-track', 'ruler', 'file-label', 'clip-inner'):
            spec = GUARDS[name]
            if name == 'clip-inner':
                spec = dict(spec, region=[spec['region'][0] + delta, *spec['region'][1:]])
            guard(frame, 'post-' + name, spec)
        if delta == 20:
            guard(frame, 'post-moved-start-gap', GUARDS['moved-gap'])
        if preview:
            guard(frame, 'post-preview-restored', GUARDS['preview'])

    def fresh(name, delta, exact=False, preview=False, success_notice=False):
        data = checked(name)
        valid = baseline_matches(data['nodes'], rows) if exact else semantic_clip_matches(data['nodes'], delta, saved=success_notice)
        if not valid:
            raise stop('post_insert_fresh_semantic_guard_changed_no_input')
        frame_guards(pixels(), delta, preview)
        focus()
        return data

    def observe(name, delta=None, exact=False, saved=False, menu=False):
        prior = None
        for sample in range(1, 5):
            data = checked(f'{name}-sample-{sample:02d}-public', allow_dialog=menu)
            valid = menu or (baseline_matches(data['nodes'], rows) if exact else semantic_clip_matches(data['nodes'], delta, saved=saved))
            if not valid:
                report.setdefault('post_readonly_pending', []).append({'phase': name, 'sample': sample})
                prior = None
                pause()
                continue
            frame = pixels()
            if not menu:
                try:
                    frame_guards(frame, delta, preview=exact)
                except stop:
                    snapshot(name)
                    raise
            record = manifest(args.work_dir) if saved else None
            state = (canonical(rows(data['nodes'])), hashlib.sha256(frame).hexdigest(),
                     (record['bytes'], record['sha256']) if record else None)
            if state == prior:
                report.setdefault('post_stable_observations', []).append(name)
                if saved:
                    report['project_manifest_after_save'] = record
                    report['save_completion_from_file_inferred'] = False
                snapshot(name, allow_dialog=menu)
                return data
            prior = state
            pause()
        snapshot(name, allow_dialog=menu)
        raise stop('post_insert_result_unconfirmed_no_further_input:' + name)

    fresh('17-before-move-public', 0, exact=True, preview=True)
    fixture(args.input_dir)
    if remaining() < 18:
        raise stop('post_insert_move_release_reserve_required')
    report['actions'].append({'kind': 'one_clip_horizontal_move_attempt', 'from': [285, 884],
                              'to': [305, 884], 'delta_pixels': 20, 'expected_seconds': 1})
    command(['xdotool', 'mousemove', '--window', str(args.window_id), '285', '884'])
    focus()
    if remaining() < 18:
        raise stop('post_insert_move_release_reserve_required')
    # Release exactly once even if a down or motion command partially fails.
    def bounded_release():
        result = subprocess.run(['xdotool', 'mouseup', '1'], capture_output=True, timeout=1)
        if result.returncode:
            raise stop('post_insert_final_release_failed_no_retry')
    try:
        command(['xdotool', 'mousedown', '1'])
        command(['xdotool', 'mousemove', '--window', str(args.window_id), '305', '884'])
    finally:
        (release or bounded_release)()
        report['move_pointer_released'] = True
    command(['xdotool', 'mousemove', '--window', str(args.window_id), '100', '650'])
    pause()
    observe('17-move-result', 20)
    report['move_visible_translation_observed'] = True
    fresh('18-before-undo-public', 20)
    report['actions'].append({'kind': 'one_source_guarded_shortcut', 'key': 'ctrl+z'})
    command(['xdotool', 'key', '--clearmodifiers', 'ctrl+z'])
    pause()
    observe('18-undo-result', 0, exact=True)
    report['undo_restored_observed_baseline'] = True
    fresh('19-before-save-public', 0, exact=True, preview=True)
    report['actions'].append({'kind': 'one_source_guarded_shortcut', 'key': 'ctrl+s'})
    command(['xdotool', 'key', '--clearmodifiers', 'ctrl+s'])
    pause()
    observe('19-save-result', 0, saved=True)
    fixture(args.input_dir)
    report['fixture_source_unchanged_after_move_undo_save'] = True
    fresh('20-before-file-public', 0, preview=True, success_notice=True)
    report['actions'].append({'kind': 'one_guarded_File_menu_observation_attempt', 'xy': [83, 16]})
    command(['xdotool', 'mousemove', '--window', str(args.window_id), '83', '16'])
    focus()
    command(['xdotool', 'click', '1'])
    command(['xdotool', 'mousemove', '--window', str(args.window_id), '100', '650'])
    pause()
    observe('20-file-menu-observed', menu=True)
    report['status'] = 'clip_post_insert_File_observed_main_review_required'
    report['close_open_export_attempted'] = False
    raise stop('unknown_File_menu_review_required_no_Close_Open_input')


if __name__ == '__main__':
    raise SystemExit(extended_probe().main())
