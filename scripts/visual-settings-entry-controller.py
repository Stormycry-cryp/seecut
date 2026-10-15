#!/usr/bin/env python3
"""Reviewed quick entry -> one named global Settings Action -> metadata-only STOP."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time

HEAD = '0aa9406247e53f073c0b4df686adc68b40e2f8f6'
APP_SHA = '03fb1752c34adb0c0f4a21521c2302ee80c9b23203728cbcf636d3a4a57eaea2'
IDENTITY_SHA = '9248168645ce9324be4cfab71e8e9d851c14e0a784050a6fc84dad68599d0a7e'
HELPER = 'visual-settings-entry-public.py'


def load_verified(path, digest):
    if (path.name != HELPER or path.is_symlink() or not path.is_file() or path.stat().st_size > 65536
            or hashlib.sha256(path.read_bytes()).hexdigest() != digest):
        raise ValueError('exact_settings_helper_copy_SHA_required')
    spec = importlib.util.spec_from_file_location('visual_settings_public', path)
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    return module


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('app-pid','window-id'):
        p.add_argument('--'+name,type=int,required=True)
    for name in ('work-dir','output','identity-approval','ui-approval'):
        p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--expected-sha',required=True)
    p.add_argument('--deadline-monotonic',type=float,required=True)
    p.add_argument('--private-accessibility-bus',action='store_true')
    p.add_argument('--isolated-display-capture',action='store_true')
    p.add_argument('--probe-python',default='/usr/bin/python3')
    args = p.parse_args()
    started = time.monotonic(); end = min(args.deadline_monotonic-15, started+120)
    directory = None; helper = None; settings_attempted = False
    report = dict(schema='seecut-visual-settings-entry-v1',scope='visual-settings-entry-observation',
        status='blocked',source_head=HEAD,app_pid=args.app_pid,window=args.window_id,sourcecopy_verified=False,actions=[],captures=[],public_metadata=[],guards=[],
        field_values_read=False,text_or_value_interfaces_read=False,editable_or_descendant_names_read=False,
        settings_pixels_captured=False,theme_Action_attempted=False,account_configuration_read=False,
        product_verdict='Settings_structure_pending_main_review',cleanup_owner='launcher <=300s')
    def command(argv,binary=False,search=False):
        left = end-time.monotonic()
        if left < .25:
            raise helper.Stop('deadline_no_input_or_read')
        result = subprocess.run(argv,capture_output=True,text=not binary,timeout=min(8,left))
        if result.returncode and not (search and result.returncode == 1):
            raise helper.Stop('owned_command_failed_no_retry_raw_withheld')
        return result.stdout
    def focus():
        helper.owned_window(command,args.app_pid,args.window_id)
    def pixels():
        if settings_attempted:
            raise helper.Stop('Settings_pixels_forbidden_after_Action_boundary')
        focus()
        return command(['import','-window',str(args.window_id),'-depth','8','rgb:-'],binary=True)
    def invoke(intent,mode,name=None):
        focus()
        argv=[args.probe_python,'-B',str(args.work_dir/HELPER),'--app-pid',str(args.app_pid),
            '--window-id',str(args.window_id),'--work-dir',str(args.work_dir),'--output',str(directory),
            '--ui-approval',str(args.ui_approval),'--deadline-monotonic',str(min(end,time.monotonic()+10)),
            '--private-accessibility-bus','--intent',intent,'--mode',mode]
        if name is not None:
            argv.extend(['--output-name',name])
        left = end-time.monotonic()
        if left < .25:
            raise helper.Stop('deadline_before_owned_helper')
        result = subprocess.run(argv,capture_output=True,timeout=min(12,left))
        if name is not None and (directory/name).is_file():
            report['public_metadata'].append(name)
        if result.returncode:
            raise helper.Stop('Settings_scope_helper_stopped_no_retry')
        if name is not None:
            path = directory/name
            if path.is_symlink() or not path.is_file() or path.stat().st_size > 131072:
                raise helper.Stop('bounded_public_metadata_required')
            data=json.loads(path.read_bytes()); helper.context(data,args.app_pid,before=mode=='before')
            return data
    try:
        if (args.expected_sha != HEAD or args.app_pid < 2 or args.window_id < 1 or end <= started
                or not args.private_accessibility_bus or not args.isolated_display_capture or not os.environ.get('DISPLAY')):
            raise ValueError('exact_private_current_runtime_required')
        for path in (args.work_dir,args.output):
            if (not path.is_absolute() or path.is_symlink() or not path.is_dir() or path.stat().st_uid != os.getuid()
                    or any(parent.is_symlink() for parent in path.parents)):
                raise ValueError('owned_real_launcher_directories_required')
        for path in (args.ui_approval,args.identity_approval):
            if not path.is_absolute() or path.is_symlink() or not path.is_file() or path.stat().st_size > 16384:
                raise ValueError('bounded_explicit_declarations_required')
        ui=json.loads(args.ui_approval.read_bytes())
        helper=load_verified(args.work_dir/HELPER,ui.get('public_sha256'))
        helper.validate_ui(ui,Path(__file__))
        identity=helper.read_decl(args.identity_approval)
        if (hashlib.sha256(args.identity_approval.read_bytes()).hexdigest()!=IDENTITY_SHA
                or identity.get('schema')!=2 or identity.get('reviewed_by')!='main-reviewer'
                or identity.get('runtime_head')!=HEAD or identity.get('runtime_app_sha256')!=APP_SHA
                or identity.get('change_scope')!='product-candidate'):
            raise helper.Stop('exact_current_reviewed_runtime_identity_required')
        proc=Path('/proc')/str(args.app_pid)
        if proc.stat().st_uid!=os.getuid() or (proc/'exe').resolve().parent!=args.work_dir:
            raise helper.Stop('owned_fresh_launcher_App_path_required')
        digest=hashlib.sha256()
        with (proc/'exe').open('rb') as stream:
            while True:
                if time.monotonic()>=end:
                    raise helper.Stop('deadline_verifying_App')
                block=stream.read(1048576)
                if not block: break
                digest.update(block)
        if digest.hexdigest()!=APP_SHA:
            raise helper.Stop('current_App_SHA_required')
        report.update(app_sha256=digest.hexdigest(),sourcecopy_verified=True,
            identity_sha256=IDENTITY_SHA,ui_sha256=hashlib.sha256(args.ui_approval.read_bytes()).hexdigest())
        directory=args.output/helper.DIRECTORY; directory.mkdir(mode=0o700)
        dims=list(map(int,command(['xdotool','getdisplaygeometry']).split()))
        if len(dims)!=2 or not (1440<=dims[0]<=1920 and 900<=dims[1]<=1200):
            raise helper.Stop('bounded_private_display_required')
        command(['xdotool','windowfocus','--sync',str(args.window_id)])
        command(['xdotool','windowsize','--sync',str(args.window_id),'1280','900'])
        if end-time.monotonic()<1:
            raise helper.Stop('deadline_before_initial_observation')
        time.sleep(.7)
        invoke('observe','before','00-before-quick-public.json')
        command(['xdotool','mousemove','--window',str(args.window_id),'100','650'])
        if not helper.rgb_matches(pixels(),ui['mode_guard']):
            raise helper.Stop('reviewed_initial_mode_RGB_required')
        report['guards'].append(dict(name='initial-mode',matched=True,**ui['mode_guard']))
        focus()
        command(['xdotool','mousemove','--window',str(args.window_id),'558','500'])
        focus()
        report['actions'].append({'kind':'one_guarded_quick_click_attempt','xy':[558,500]})
        command(['xdotool','click','1'])
        if end-time.monotonic()<1:
            raise helper.Stop('deadline_before_quick_observation')
        time.sleep(.7)
        helper.entry_target(invoke('observe','before','01-after-quick-public.json'),args.app_pid)
        command(['xdotool','mousemove','--window',str(args.window_id),'100','650'])
        if not helper.rgb_matches(pixels(),ui['settings_guard']):
            raise helper.Stop('current_named_Settings_ROI_required')
        report['guards'].append(dict(name='named-Settings-entry',matched=True,**ui['settings_guard']))
        helper.entry_target(invoke('observe','before','02-before-settings-public.json'),args.app_pid)
        # Set before invoking the helper, including helper failure/false Action. No pixel function thereafter.
        settings_attempted=True
        report['actions'].append({'kind':'one_named_Settings_Action_attempt','path':[0,6]})
        invoke('open-settings','before')
        data=invoke('observe','settings','03-settings-entry-public.json')
        safe=[n for n in data['nodes'] if n.get('label') is not None]
        report['safe_static_controls']=[{k:n.get(k) for k in ('path','role','label','bounds','checked','focused','allowed_actions')} for n in safe]
        report['status']='global_Settings_metadata_observed_STOP_main_review_required'
    except Exception as exc:
        report['blocking_reason']=str(exc) if helper is not None and isinstance(exc,helper.Stop) else 'private_scope_runtime_error_raw_withheld'
    finally:
        report['elapsed_seconds']=round(time.monotonic()-started,3)
        if directory is not None and helper is not None:
            helper.write_record(directory,'visual-settings-report.json',report,16384)
        print(json.dumps({k:report[k] for k in ('scope','status','blocking_reason','sourcecopy_verified') if k in report}))
    return 0 if report['status']=='global_Settings_metadata_observed_STOP_main_review_required' else 2


if __name__ == '__main__':
    raise SystemExit(main())
