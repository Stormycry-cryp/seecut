#!/usr/bin/env python3
"""Keep the native business surface inventory explicit as source code changes.

This is source coverage only. A registered tool is never counted as runtime,
model, UI, or finished-artifact acceptance.
"""
import argparse
import ast
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / 'docs/agent/feature-coverage.json'
COMMANDS = 'src/crates/concat-project/src/commands/mod.rs'
CANVAS = 'src/crates/concat/src/panes/canvas.rs'
MCP = 'src/crates/concat-editor-mcp/src/main.rs'
UI = 'src/crates/concat/ui'


def read(path):
    return (ROOT / path).read_text()


def scan():
    commands = read(COMMANDS).split('pub enum Command {', 1)[1].split('\n}', 1)[0]
    result = {}
    for name in re.findall(r'^    ([A-Z]\w*)\s*(?:\{|,)', commands, re.M):
        result[f'command:{name}'] = {'source': COMMANDS, 'symbol': name, 'kind': 'nativeCommand'}
    for name in re.findall(r'pub fn (agent_\w+)\s*\(', read(CANVAS)):
        result[f'canvas:{name}'] = {'source': CANVAS, 'symbol': name, 'kind': 'nativeHelper'}
    for name in re.findall(r'#\[tool\([\s\S]*?\)\]\s*(?:async\s+)?fn (\w+)\s*\(', read(MCP)):
        result[f'tool:{name}'] = {'source': MCP, 'symbol': name, 'kind': 'registeredTool'}
    # Record UI declarations separately: gestures and repeated component events
    # still need mapping to actual business functions, not a coverage percentage.
    ui = {}
    for path in sorted((ROOT / UI).rglob('*.slint')):
        names = sorted(set(re.findall(r'^\s*(?:pure )?callback ([\w-]+)\s*\(', path.read_text(), re.M)))
        if names:
            ui[path.relative_to(ROOT).as_posix()] = names
    return result, ui


def update():
    observed, ui = scan()
    previous = json.loads(CATALOG.read_text()) if CATALOG.exists() else {}
    rows = previous.get('entries', {})
    result = {}
    for key, entry in sorted(observed.items()):
        old = rows.get(key, {})
        result[key] = {
            **entry,
            'module': old.get('module', 'clip' if entry['kind'] == 'nativeCommand' else 'canvas' if entry['kind'] == 'nativeHelper' else 'mcp'),
            'mcpTools': old.get('mcpTools', [entry['symbol']] if entry['kind'] == 'registeredTool' else []),
            'agentStatus': old.get('agentStatus', 'registered' if entry['kind'] == 'registeredTool' else 'unmapped'),
            'acceptance': old.get('acceptance', 'pending'),
            'evidence': old.get('evidence', []),
        }
    CATALOG.parent.mkdir(parents=True, exist_ok=True)
    CATALOG.write_text(json.dumps({
        'version': 1,
        'scope': 'Observed native commands, canvas helpers, MCP registrations, and Slint callback declarations. Generic action/changed routes and business grouping are audited separately. This is not a complete business denominator.',
        'entries': result,
        'uiDeclarations': ui,
        'businessRoutes': previous.get('businessRoutes', []),
    }, ensure_ascii=False, indent=2) + '\n')


def check():
    catalog = json.loads(CATALOG.read_text())
    observed, ui = scan()
    entries = catalog['entries']
    errors = []
    for key in sorted(set(observed) - set(entries)):
        errors.append(f'Untracked native surface: {key}')
    for key in sorted(set(entries) - set(observed)):
        errors.append(f'Stale native surface: {key}')
    tools = {entry['symbol'] for entry in observed.values() if entry['kind'] == 'registeredTool'}
    host = read('src/crates/concat/src/editor_mcp.rs')
    advertised = re.search(r'"tools"\s*:\s*(\[[^\]]*\])', host)
    if not advertised or set(json.loads(advertised.group(1))) != tools:
        errors.append('Native capability advertisement differs from MCP registrations.')
    server = read('services/seecut-server/src/seecut_server/assistant_contract.py')
    allowed = re.search(r'^TOOLS\s*=\s*frozenset\((\{[^}]+\})\)', server, re.M)
    if not allowed or ast.literal_eval(allowed.group(1)) != tools:
        errors.append('Managed Agent service tool vocabulary differs from MCP registrations.')
    for key, row in entries.items():
        for field in ['source', 'symbol', 'kind']:
            if key in observed and row.get(field) != observed[key][field]:
                errors.append(f'Source identity changed: {key}.{field}')
        if any(name not in tools for name in row['mcpTools']):
            errors.append(f'Unknown MCP registration: {key}')
        if row['acceptance'] not in ['pending', 'source', 'logic', 'app', 'task']:
            errors.append(f'Invalid acceptance: {key}')
        if row['acceptance'] != 'pending' and not row['evidence']:
            errors.append(f'Missing evidence: {key}')
        for evidence in row['evidence']:
            if not (ROOT / evidence).is_file():
                errors.append(f'Missing evidence file: {key}: {evidence}')
    if catalog['uiDeclarations'] != ui:
        errors.append('Slint callback declarations changed; update the inventory and audit the affected business route.')
    route_ids = set()
    for route in catalog.get('businessRoutes', []):
        required = ['id', 'module', 'label', 'source', 'actions', 'mcpTools', 'agentStatus', 'acceptance', 'notes']
        if any(key not in route for key in required):
            errors.append('Business route is missing required coverage fields.')
            continue
        if route['id'] in route_ids:
            errors.append(f'Duplicate business route: {route["id"]}')
        route_ids.add(route['id'])
        if route['agentStatus'] not in ['unmapped', 'partial', 'registered']:
            errors.append(f'Invalid business route status: {route["id"]}')
        if route['acceptance'] not in ['pending', 'source', 'logic', 'app', 'task']:
            errors.append(f'Invalid business route acceptance: {route["id"]}')
        if route['acceptance'] in ['logic', 'app', 'task'] and not route.get('evidence'):
            errors.append(f'Missing business route evidence: {route["id"]}')
        for evidence in route.get('evidence', []):
            if not (ROOT / evidence).is_file():
                errors.append(f'Missing business route evidence file: {route["id"]}: {evidence}')
        if not (ROOT / route['source']).is_file():
            errors.append(f'Missing business route source: {route["source"]}')
        for name in route.get('mcpTools', []):
            if name not in tools:
                errors.append(f'Unknown business route tool: {route["id"]}: {name}')
    for error in errors:
        print(error)
    counts = {kind: sum(row['kind'] == kind for row in entries.values()) for kind in ['nativeCommand', 'nativeHelper', 'registeredTool']}
    print(json.dumps({'inventory': counts, 'uiFiles': len(ui), 'uiDeclarations': sum(map(len, ui.values())), 'businessRoutes': len(catalog.get('businessRoutes', [])), 'completeBusinessDenominator': False, 'errors': len(errors)}, ensure_ascii=False))
    return bool(errors)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--update-inventory', action='store_true', help='Refresh observed declarations, retaining recorded acceptance; does not mark any feature complete.')
    args = parser.parse_args()
    if args.update_inventory:
        update()
    raise SystemExit(check())
