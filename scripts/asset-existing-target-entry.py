#!/usr/bin/env python3
"""One existing-target modal entry after the exact reviewed7be return; no selection."""
import hashlib
import json

RETURN_NODES_SHA='0d2a8d07d77d591b557f980814b70deba4ad673f99dfe0f6a4dda600327efb1e'
RETURN_GUARD={'region':[0,0,1280,900],
 'rgb_sha256':'ac6d4bbba760860ad7dc46209d4536d9a7dcb3d3dcadd1150c35638baf460847'}
TARGET={'path':[0,28],'role':43,'label':'加入画布','showing':True,
 'enabled':True,'sensitive':True,'focused':False,'focusable':True,
 'selected':False,'checked':False,'pressed':False,'modal':False,
 'file_chooser':False,'dialog':False,'button':True,'radio':False,'panel':False,
 'entry':False,'editable':False,'editable_text_interface':False,
 'action_interface':True,'allowed_actions':['click'],
 'bounds':{'x':1016,'y':184,'width':40,'height':40}}


def returned_target(data,pid,public_context,Stop):
    showing=public_context(data,pid)
    nodes=data['nodes']
    digest=hashlib.sha256(json.dumps(nodes,ensure_ascii=False,sort_keys=True,
                                    separators=(',',':')).encode()).hexdigest()
    if len(nodes)!=51 or digest!=RETURN_NODES_SHA:
        raise Stop('existing_entry_reviewed_return_tree_changed_no_input')
    targets=[n for n in showing if n.get('label')=='加入画布' and n.get('role')==43]
    if len(targets)!=1 or targets[0]!=TARGET:
        raise Stop('existing_entry_unique_return_canvas_Action_required')
    return targets[0]


SEARCH_TEMPLATE={'path': [0, 16], 'role': 79, 'label': None, 'showing': True, 'enabled': True, 'sensitive': True, 'focused': False, 'focusable': True, 'selected': False, 'checked': False, 'pressed': False, 'modal': False, 'file_chooser': False, 'dialog': False, 'button': False, 'radio': False, 'panel': False, 'entry': True, 'editable': True, 'editable_text_interface': False, 'action_interface': False, 'bounds': {'x': 930, 'y': 79, 'width': 212, 'height': 14}}
COPY_GROUP_TEMPLATE=[{'path': [0, 40], 'role': 43, 'label': 'Copy', 'showing': True, 'enabled': True, 'sensitive': True, 'focused': False, 'focusable': True, 'selected': False, 'checked': False, 'pressed': False, 'modal': False, 'file_chooser': False, 'dialog': False, 'button': True, 'radio': False, 'panel': False, 'entry': False, 'editable': False, 'editable_text_interface': False, 'action_interface': True, 'allowed_actions': ['click'], 'bounds': {'x': 1206, 'y': 870, 'width': 52, 'height': 20}}, {'path': [0, 40, 0], 'role': 79, 'label': None, 'showing': True, 'enabled': False, 'sensitive': False, 'focused': False, 'focusable': True, 'selected': False, 'checked': False, 'pressed': False, 'modal': False, 'file_chooser': False, 'dialog': False, 'button': False, 'radio': False, 'panel': False, 'entry': True, 'editable': False, 'editable_text_interface': False, 'action_interface': False, 'bounds': {'x': 1206, 'y': 870, 'width': 1, 'height': 1}}, {'path': [0, 40, 1], 'role': 29, 'label': 'Copy', 'showing': True, 'enabled': True, 'sensitive': True, 'focused': False, 'focusable': False, 'selected': False, 'checked': False, 'pressed': False, 'modal': False, 'file_chooser': False, 'dialog': False, 'button': False, 'radio': False, 'panel': False, 'entry': False, 'editable': False, 'editable_text_interface': False, 'action_interface': False, 'bounds': {'x': 1220, 'y': 875, 'width': 24, 'height': 10}}]
BOOL_KEYS=frozenset(('showing','enabled','sensitive','focused','focusable','selected',
 'checked','pressed','modal','file_chooser','dialog','button','radio','panel','entry',
 'editable','editable_text_interface','action_interface'))
NODE_REQUIRED=BOOL_KEYS|{'path','role','label'}


def observation_schema(nodes,Stop):
    for n in nodes:
        if (not NODE_REQUIRED<=set(n) or not set(n)<=NODE_REQUIRED|{'bounds','allowed_actions'}
                or any(type(n[k]) is not bool for k in BOOL_KEYS)
                or type(n['role']) is not int or n['role']<0
                or not (n['label'] is None or isinstance(n['label'],str))):
            raise Stop('existing_entry_node_schema_no_pixels')
        if ('allowed_actions' in n)!=((n['button'] or n['radio']) and n['action_interface']):
            raise Stop('existing_entry_Action_schema_no_pixels')
        if 'allowed_actions' in n:
            actions=n['allowed_actions']
            if (not isinstance(actions,list) or len(actions)>8
                    or any(x not in ('click','activate','press') for x in actions)):
                raise Stop('existing_entry_Action_schema_no_pixels')
        if 'bounds' in n:
            b=n['bounds']
            if (not isinstance(b,dict) or set(b)!={'x','y','width','height'}
                    or any(type(v) is not int for v in b.values())
                    or b['width']<0 or b['height']<0):
                raise Stop('existing_entry_bounds_schema_no_pixels')
        elif n['path']!=[] or n['role']!=75:
            raise Stop('existing_entry_bounds_schema_no_pixels')


def safe_observation(data,pid,public_context,Stop):
    """Closed field/schema observation gate; never authorize unknown modal nodes."""
    public_context(data,pid,allow_dialog=True)
    nodes=data['nodes'];observation_schema(nodes,Stop)
    if any(n['file_chooser'] for n in nodes):
        raise Stop('existing_entry_native_surface_no_pixels')
    searches=[n for n in nodes if n['path']==SEARCH_TEMPLATE['path']]
    if len(searches)!=1 or searches[0]!=SEARCH_TEMPLATE:
        raise Stop('existing_entry_exact_Search_no_pixels')
    # The only optional field is the observed disabled Copy child. Bind its
    # entire parent/two-child subtree. Only one root child index may shift.
    copy_fields=[n for n in nodes if n['role']==79 and n['path']!=SEARCH_TEMPLATE['path']]
    permitted=[SEARCH_TEMPLATE['path']]
    group_paths=[]
    if copy_fields:
        if len(copy_fields)!=1 or len(copy_fields[0]['path'])!=3 or copy_fields[0]['path'][0]!=0 or copy_fields[0]['path'][2]!=0:
            raise Stop('existing_entry_unique_Copy_group_no_pixels')
        prefix=copy_fields[0]['path'][:2]
        group=[n for n in nodes if n['path'][:2]==prefix]
        expected=[dict(n,path=prefix+n['path'][2:]) for n in COPY_GROUP_TEMPLATE]
        if len(group)!=3 or sorted(group,key=lambda n:n['path'])!=expected:
            raise Stop('existing_entry_exact_Copy_group_no_pixels')
        group_paths=[n['path'] for n in expected]
        permitted.append(prefix+[0])
    if any(n['label']=='Copy' and n['path'] not in group_paths for n in nodes):
        raise Stop('existing_entry_unique_Copy_group_no_pixels')
    by_path={tuple(n['path']):n for n in nodes}
    for n in nodes:
        if (n['entry'] or n['editable'] or n['editable_text_interface'] or n['role']==79) and n['path'] not in permitted:
            raise Stop('existing_entry_unknown_field_no_pixels')
    for path in permitted:
        for depth in range(len(path)):
            ancestor=by_path.get(tuple(path[:depth]))
            if ancestor is None or ancestor['entry'] or ancestor['editable'] or ancestor['editable_text_interface']:
                raise Stop('existing_entry_field_ancestor_no_pixels')


def observe(api):
    report=api['report'];Stop=api['Stop'];pid=api['pid']
    if report.get('sourcecopy_post_return_verified') is not True:
        raise Stop('existing_entry_owned_sourcecopy_required')
    for name in ('14-before-existing-target-public','14-existing-target-recheck-public'):
        target=returned_target(api['probe'](name),pid,api['public_context'],Stop)
        api['guard'](api['pixels'](),'reviewed-return-assets-full',RETURN_GUARD)
    report['current_handoff_attempted']=True
    api['app_action'](target,'canvas',14)
    report['current_handoff_Action_success']=True
    # Snapshot collects one more fresh full metadata and runs safe_observation
    # before any raw PNG. No target selection, join, restore or second Action.
    api['snapshot']('14-existing-target-modal',allow_dialog=True)
    report['status']='asset_existing_target_entry_result_observed_main_review_required'
    report['flow_result']='after_one_existing_target_entry_Action_surface_unreviewed'
    report['existing_target_selection_attempted']=False
    report['existing_target_join_attempted']=False
