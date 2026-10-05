#!/usr/bin/env python3
"""One closed asset search edit or visible single-selection/canvas Action. No value reads."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time

MODES = ('search-miss', 'search-clear', 'batch', 'select', 'canvas')
MATCH_WORD = 'qa-no-match-8f7c2d1b'
TARGET_KEYS = ('path','role','label','showing','enabled','sensitive','focusable','button',
               'entry','editable','editable_text_interface','action_interface','bounds','allowed_actions')



SAFE_REASONS = frozenset((
    'named_probe_SHA_required','complete_current_nonfield_metadata_required',
    'one_current_unmodal_asset_surface_required','current_owned_App_window_required',
    'unknown_field_overlay_no_input','current_visible_target_changed',
    'healthy_focusable_target_required','fixed_nonreadable_search_entry_required',
    'unique_current_search_focus_required','closed_visible_nonfield_Action_required',
    'one_current_owned_asset_required','fresh_exact_single_selection_count_required',
    'one_current_selected_preview_required','observed_unchecked_batch_control_required',
    'unique_preview_select_button_required','closed_search_mode_required',
    'final_nonfield_closed_Action_state_required','final_Action_bounds_changed',
    'one_advertised_click_required','deadline_before_Action','Action_false_no_retry',
    'deadline_no_input','owned_command_failed','owned_App_focus_required',
    'task_limit','descendant_limit','extra_owned_window_forbidden',
    'exact_private_owned_context_required','exact_scope_action_SHA_required',
    'no_file_list_path_required','visible_target_missing',
    'public_accessibility_unavailable','public_accessibility_deadline',
    'ambiguous_public_app_root','target_not_exposed_by_public_accessibility',
    'probe_node_state_set_missing','probe_node_interfaces_missing','probe_node_action_iface_missing',
    'probe_node_interfaces_invalid','post_edit_probe_transient_exhausted',
    'post_edit_probe_deadline','unknown_probe_transient_code'))


def safe_reason(exc):
    if isinstance(exc,subprocess.TimeoutExpired):return 'owned_command_timeout'
    reason=str(exc)
    return reason if isinstance(exc,(ValueError,RuntimeError)) and reason in SAFE_REASONS else 'unknown_runtime_failure_raw_withheld'



POST_EDIT_TRANSIENT_CODES = frozenset((
    'probe_node_state_set_missing','probe_node_interfaces_missing',
    'probe_node_action_iface_missing'))


def stable_post_edit(probe, pid, end, focus, validate, record,
                     now=time.monotonic, wait=time.sleep):
    """At most three READ-ONLY samples, only mandatory interface None is recoverable.

    Every collect takes a new current App root; no input/click/Action or cached
    snapshot is replayed. Unknown errors, incomplete coverage and failed target
    assertions are terminal. Deadline and focus checks remain mandatory.
    """
    record['post_edit_probe_samples']=0
    record['post_edit_probe_codes']=[]
    for attempt in range(3):
        if end-now()<.1:raise ValueError('post_edit_probe_deadline')
        record['phase']='post_edit_focus'
        focus()
        record['phase']='post_edit_probe'
        record['post_edit_probe_samples']=attempt+1
        try:
            data=probe.collect(pid,min(end,now()+2))
        except probe.ProbeInterfaceUnavailable as exc:
            code=str(exc)
            if code not in POST_EDIT_TRANSIENT_CODES:
                raise ValueError('unknown_probe_transient_code') from None
            record['post_edit_probe_codes'].append(code)
            if attempt==2:raise ValueError('post_edit_probe_transient_exhausted') from None
            delay=.05*(attempt+1)
            if end-now()<delay+.1:raise ValueError('post_edit_probe_deadline') from None
            record['phase']='post_edit_probe_backoff'
            wait(delay)
            continue
        record['post_edit_probe_completed']=True
        record['phase']='post_edit_focus_after_probe'
        focus()
        record['phase']='post_edit_target_check'
        return validate(data)
    raise ValueError('post_edit_probe_transient_exhausted')


def load_probe(digest):
    path=Path(__file__).with_name('asset-flow-public-probe.py')
    if (path.is_symlink() or not path.is_file() or path.stat().st_size>65536
            or hashlib.sha256(path.read_bytes()).hexdigest()!=digest):
        raise ValueError('named_probe_SHA_required')
    spec=importlib.util.spec_from_file_location('asset_flow_probe',path)
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    return module


def target_current(data,pid,expected,mode,focused=False):
    if (mode not in MODES or data.get('coverage_complete') is not True
            or data.get('app_pid')!=pid or data.get('field_values_read') is not False):
        raise ValueError('complete_current_nonfield_metadata_required')
    nodes=data.get('nodes',[]);showing=[n for n in nodes if n.get('showing')]
    if (not 1<=len(nodes)<=512 or len({tuple(n['path']) for n in nodes})!=len(nodes)
            or any(n.get('modal') or n.get('dialog') or n.get('file_chooser') for n in showing)):
        raise ValueError('one_current_unmodal_asset_surface_required')
    windows=[n for n in showing if n.get('role')==23]
    if (len(windows)!=1 or windows[0]['path']!=[0]
            or windows[0].get('bounds')!={'x':0,'y':0,'width':1280,'height':900}
            or not all(windows[0].get(k) is True for k in ('enabled','sensitive'))):
        raise ValueError('current_owned_App_window_required')
    if any(n.get('editable') and n['path']!=[0,16] and not (
            n.get('bounds',{}).get('width')==1 and n.get('bounds',{}).get('height')==1
            and n.get('bounds',{}).get('x')==1206 and n.get('bounds',{}).get('y',-1)>=840
            and n.get('enabled') is False and n.get('sensitive') is False) for n in showing):
        raise ValueError('unknown_field_overlay_no_input')
    found=[n for n in showing if n['path']==expected.get('path')]
    if len(found)!=1 or any(found[0].get(k)!=expected.get(k) for k in TARGET_KEYS):
        raise ValueError('current_visible_target_changed')
    node=found[0]
    if not all(node.get(k) is True for k in ('enabled','sensitive','focusable')):
        raise ValueError('healthy_focusable_target_required')
    if mode.startswith('search-'):
        if (node['path']!=[0,16] or node.get('role')!=79 or node.get('label') is not None
                or node.get('bounds')!={'x':930,'y':79,'width':212,'height':14}
                or not node.get('entry') or not node.get('editable') or node.get('editable_text_interface')):
            raise ValueError('fixed_nonreadable_search_entry_required')
        if focused:
            fields=[n for n in showing if n.get('focused') and n.get('entry') and n.get('editable')]
            if len(fields)!=1 or fields[0]['path']!=node['path']:
                raise ValueError('unique_current_search_focus_required')
    else:
        labels={'batch':'批量管理','select':'选择素材','canvas':'加入画布'}
        if (node.get('label')!=labels[mode] or node.get('entry') or node.get('editable')
                or node.get('editable_text_interface') or not node.get('action_interface')):
            raise ValueError('closed_visible_nonfield_Action_required')
        counts=[n for n in showing if n['path']==[0,15] and n.get('role')==29 and n.get('label')=='1 项']
        if len(counts)!=1:raise ValueError('one_current_owned_asset_required')
        if mode in ('select','canvas'):
            wanted='已选 0 项' if mode=='select' else '已选 1 项'
            forbidden='已选 1 项' if mode=='select' else '已选 0 项'
            if (len([n for n in showing if n.get('role')==29 and n.get('label')==wanted])!=1
                    or any(n.get('label')==forbidden for n in showing)):
                raise ValueError('fresh_exact_single_selection_count_required')
            if mode=='canvas':
                previews=[n for n in showing if n.get('role')==43 and n.get('label')=='取消选择'
                          and n.get('button') and n.get('action_interface') and n.get('allowed_actions')==['click']]
                if len(previews)!=1:raise ValueError('one_current_selected_preview_required')
        if mode=='batch' and (node['path']!=[0,19] or node['role']!=62
                or node['bounds']!={'x':1208,'y':66,'width':40,'height':40} or node.get('checked')):
            raise ValueError('observed_unchecked_batch_control_required')
        if mode=='select' and (node['role']!=43 or node.get('allowed_actions')!=['click']):
            raise ValueError('unique_preview_select_button_required')
    return node


def search_sequence(mode,target,focus,observe,command,record):
    if mode not in ('search-miss','search-clear'):raise ValueError('closed_search_mode_required')
    focus();b=target['bounds'];record['phase']='focus_click';record['focus_click_attempted']=True
    command(['xdotool','mousemove','--window',str(record['window']),str(b['x']+b['width']//2),str(b['y']+b['height']//2)])
    focus();command(['xdotool','click','1'])
    record['phase']='focus_before_select';observe(True);focus();record['select_all_attempted']=True
    command(['xdotool','key','--clearmodifiers','ctrl+a']);observe(True);focus()
    record['phase']='fixed_edit';record['edit_attempted']=True
    if mode=='search-miss':command(['xdotool','type','--clearmodifiers','--delay','1','--',MATCH_WORD])
    else:command(['xdotool','key','--clearmodifiers','BackSpace'])
    record['phase']='post_edit_observation'
    observe(True)
    record['post_edit_target_verified']=True


def once_action(node,Atspi,target,focus,record,end):
    state=node.get_state_set();interfaces=set(node.get_interfaces())
    if (node.get_role()!=target['role'] or state.contains(Atspi.StateType.EDITABLE)
            or 'EditableText' in interfaces or 'Action' not in interfaces
            or not all(state.contains(s) for s in (Atspi.StateType.SHOWING,Atspi.StateType.ENABLED,Atspi.StateType.SENSITIVE))
            or node.get_name()!=target['label']):
        raise ValueError('final_nonfield_closed_Action_state_required')
    b=node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
    if {'x':int(b.x),'y':int(b.y),'width':int(b.width),'height':int(b.height)}!=target['bounds']:
        raise ValueError('final_Action_bounds_changed')
    action=node.get_action_iface();count=action.get_n_actions()
    if count!=1 or action.get_action_name(0)!='click':raise ValueError('one_advertised_click_required')
    focus()
    if time.monotonic()>=end:raise ValueError('deadline_before_Action')
    record['phase']='Action_once';record['Action_attempted']=True
    if not action.do_action(0):raise ValueError('Action_false_no_retry')


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for n in ('app-pid','window-id'):p.add_argument('--'+n,type=int,required=True)
    for n in ('work-dir','ui-approval'):p.add_argument('--'+n,type=Path,required=True)
    p.add_argument('--node-public',required=True);p.add_argument('--mode',choices=MODES,required=True)
    p.add_argument('--deadline-monotonic',type=float,required=True)
    p.add_argument('--private-accessibility-bus',action='store_true');a=p.parse_args()
    end=min(a.deadline_monotonic,time.monotonic()+8)
    record={'scope':'asset-library-flow','mode':a.mode,'success':False,'phase':'dependencies',
            'window':a.window_id,'field_values_read':False,'file_lists_read':False,
            'focus_click_attempted':False,'select_all_attempted':False,'edit_attempted':False,'Action_attempted':False,
            'post_edit_probe_completed':False,'post_edit_target_verified':False,
            'post_edit_probe_samples':0,'post_edit_probe_codes':[]}
    def command(argv,search=False):
        left=end-time.monotonic()
        if left<0.1:raise ValueError('deadline_no_input')
        r=subprocess.run(argv,capture_output=True,text=True,timeout=min(3,left))
        if r.returncode and not(search and r.returncode==1):raise ValueError('owned_command_failed')
        return r.stdout
    def focus():
        if (int(command(['xdotool','getwindowpid',str(a.window_id)]).strip())!=a.app_pid
                or int(command(['xdotool','getwindowfocus']).strip())!=a.window_id
                or set(command(['xdotool','search','--onlyvisible','--pid',str(a.app_pid)],True).split())!={str(a.window_id)}):
            raise ValueError('owned_App_focus_required')
        pending,seen=[a.app_pid],{a.app_pid}
        while pending:
            tasks=list((Path('/proc')/str(pending.pop())/'task').iterdir())
            if len(tasks)>64:raise ValueError('task_limit')
            for task in tasks:
                try:children=list(map(int,(task/'children').read_text().split()))
                except FileNotFoundError:continue
                for child in children:
                    if child not in seen and probe.owned_by_app(child,a.app_pid):
                        if len(seen)>=24:raise ValueError('descendant_limit')
                        seen.add(child);pending.append(child)
                        if command(['xdotool','search','--onlyvisible','--pid',str(child)],True).strip():
                            raise ValueError('extra_owned_window_forbidden')
    def observe(focused=False):
        def validate(data):
            data.update(app_pid=a.app_pid,field_values_read=False)
            return target_current(data,a.app_pid,expected,a.mode,focused)
        if record['phase']=='post_edit_observation':
            return stable_post_edit(probe,a.app_pid,end,focus,validate,record)
        record['phase']='target_focus'
        focus()
        record['phase']='target_probe'
        data=probe.collect(a.app_pid,min(end,time.monotonic()+2))
        record['phase']='target_check'
        return validate(data)
    try:
        path=a.ui_approval
        if (not a.private_accessibility_bus or not os.environ.get('DISPLAY') or a.app_pid<2
                or not path.is_absolute() or path.is_symlink() or not path.is_file() or path.stat().st_size>16384
                or (Path('/proc')/str(a.app_pid)).stat().st_uid!=os.getuid()
                or (Path('/proc')/str(a.app_pid)/'exe').resolve().parent!=a.work_dir):
            raise ValueError('exact_private_owned_context_required')
        ui=json.loads(path.read_bytes())
        if (ui.get('scope')!='asset-library-flow' or ui.get('app_action_sha256')!=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()):
            raise ValueError('exact_scope_action_SHA_required')
        probe=load_probe(ui['public_probe_sha256']);expected=json.loads(a.node_public)
        target=observe();record['phase']='target_observed'
        if a.mode.startswith('search-'):search_sequence(a.mode,target,focus,observe,command,record)
        else:
            root,Atspi=probe.target_root(a.app_pid,end);node=root
            for index in target['path']:
                if (type(index) is not int or not 0<=index<128 or time.monotonic()>=end
                        or node.get_role() in (Atspi.Role.TABLE,Atspi.Role.TREE,Atspi.Role.TREE_TABLE,Atspi.Role.LIST,Atspi.Role.DIRECTORY_PANE)):
                    raise ValueError('no_file_list_path_required')
                node=node.get_child_at_index(index)
                if node is None:raise ValueError('visible_target_missing')
            node.clear_cache_single();observe();once_action(node,Atspi,target,focus,record,end)
        record.update(success=True,phase='completed')
    except Exception as exc:
        record['blocking_reason']='asset_app_input_unconfirmed_raw_withheld_no_retry'
        record['reason']=safe_reason(exc)
        if 'probe' in locals():
            diagnostic=probe.collector_failure(exc)
            if diagnostic is not None:record['collector_failure']=diagnostic
    print(json.dumps(record,ensure_ascii=False,separators=(',',':')))
    return 0 if record['success'] else 2

if __name__=='__main__':raise SystemExit(main())
