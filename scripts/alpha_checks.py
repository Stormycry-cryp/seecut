"""Fixed transparent fixture checks; no App access, text reads or UI input."""
import hashlib
import json
import os
from pathlib import Path
import stat

RGBA_SHA = '9321b2244ee4bd74dcdaaf52601784addc2245123278c0e604d6291cc5dccd31'
PNG_SHA = '8201fa2b0c7c94de97433462d40ae97aa43b77e395921769d967c7f168875b75'
TITLE = 'transparent-markers.png'
PALETTE = ((240,32,32),(32,208,64),(32,64,240),(240,208,32),(255,0,255),(0,255,255))
BOXES = ((24,24,128,96),(128,24,232,96),(24,96,128,168),(128,96,232,168),(40,40,48,48),(200,138,210,148))
COUNTS = (7280,7344,7344,7244,64,100)
CROP = (176,204,736,552)


def exact_rgba(rgba, dimensions=(256,192)):
    return dimensions == (256,192) and len(rgba) == 256*192*4 and hashlib.sha256(rgba).hexdigest() == RGBA_SHA


def crop_sha(rgb, width=1280, height=900):
    if len(rgb) != width*height*3:
        raise ValueError('unexpected_RGB_size')
    x,y,w,h = CROP
    return hashlib.sha256(b''.join(rgb[(row*width+x)*3:(row*width+x+w)*3] for row in range(y,y+h))).hexdigest()


def _groups(rgb,width,height,region):
    if len(rgb) != width*height*3:
        raise ValueError('unexpected_RGB_size')
    left,top,right,bottom = region
    if not (0 <= left < right <= width and 0 <= top < bottom <= height):
        raise ValueError('fixed_profile_region_outside_image')
    lookup = {bytes(c):i for i,c in enumerate(PALETTE)}
    groups = [[] for _ in PALETTE]
    for y in range(top,bottom):
        for x in range(left,right):
            i=lookup.get(rgb[(y*width+x)*3:(y*width+x+1)*3])
            if i is not None: groups[i].append((x,y))
    if any(len(g)<4 for g in groups):
        raise ValueError('six_fixed_markers_not_visible')
    return groups


def _profile(groups,origin,scale,tolerance=3):
    boxes = [[min(x for x,y in g),min(y for x,y in g),max(x for x,y in g)+1,max(y for x,y in g)+1] for g in groups]
    ox,oy=origin; sx,sy=scale
    for i,(box,source) in enumerate(zip(boxes,BOXES)):
        wanted=[ox+source[0]*sx,oy+source[1]*sy,ox+source[2]*sx,oy+source[3]*sy]
        if any(abs(a-b)>tolerance for a,b in zip(box,wanted)):
            raise ValueError('fixed_six_marker_layout_or_dimensions_differ')
        # Interpolation changes exact RGB edge counts; still bound each color's area.
        if i < 4:
            if abs(len(groups[i])/(COUNTS[i]*sx*sy)-1)>0.25:
                raise ValueError('fixed_marker_density_or_holes_differ')
        else:
            # Two smooth resamples may erode up to two exact-color pixels per
            # edge. Bind inner markers to their source-sized rectangle and
            # require solid exact-color interiors, not an unconstrained count.
            w=(source[2]-source[0])*sx;h=(source[3]-source[1])*sy
            area=(box[2]-box[0])*(box[3]-box[1])
            if not (max(1,w-4)*max(1,h-4)<=len(groups[i])<=(w+1)*(h+1)
                    and len(groups[i])/area >= 0.9):
                raise ValueError('fixed_inner_marker_dimensions_or_solidity_differ')
    points=[p for g in groups for p in g]
    return {'bounds':[min(x for x,y in points),min(y for x,y in points),max(x for x,y in points)+1,max(y for x,y in points)+1],
            'centroids':[[sum(x for x,y in g)/len(g),sum(y for x,y in g)/len(g)] for g in groups],
            'color_bounds':boxes,'color_pixels':[len(g) for g in groups], 'profile':'fixed-transparent-six-markers'}


def locate_fixture(rgb,width,height,region=None):
    groups=_groups(rgb,width,height,region or (0,0,width,height))
    if (width,height)==(256,192):
        result=_profile(groups,(0,0),(1,1),0)
    elif (width,height)==(1280,900):
        # Only original 288% canvas and this test's single 26/17 drag are allowed.
        for origin in ((176,204),(202,221)):
            try:
                result=_profile(groups,origin,(736/256,552/192))
                break
            except ValueError:
                pass
        else: raise ValueError('unknown_alpha_canvas_layout_no_input')
    else: raise ValueError('unknown_alpha_profile_dimensions')
    result['RGB_sha256']=hashlib.sha256(rgb).hexdigest()
    return result


def gallery_preview(rgb,left):
    if left not in (493,874):
        raise ValueError('only_two_fixed_gallery_bounds_allowed')
    # canvas.rs imageops::thumbnail forces480x320; AssetPreview cover357x214.
    # The source-backed preview transform is nonuniform, centered vertically.
    sx=357/256; sy=(320*357/480)/192
    groups=_groups(rgb,1280,900,(left,144,left+357,358))
    return _profile(groups,(left,144+(214-192*sy)/2),(sx,sy))


def translated(before,after,dx,dy):
    return (all(after['bounds'][i]-before['bounds'][i]==(dx if i%2==0 else dy) for i in range(4))
            and before['color_pixels']==after['color_pixels'])


def translated_with_canvas_clip(before,after,dx,dy):
    if (dx,dy)!=(26,17): return False
    return (all(abs(after['centroids'][i][axis]-before['centroids'][i][axis]-delta)<=2 for i in range(6) for axis,delta in ((0,dx),(1,dy)))
            and all(abs(a-b-(dx if j%2==0 else dy))<=3 for source,target in zip(before['color_bounds'],after['color_bounds']) for j,(b,a) in enumerate(zip(source,target))))


def complete_public(data,pid):
    nodes=data.get('nodes')
    if (data.get('status')!='public_metadata_observed' or data.get('coverage_complete') is not True
            or data.get('field_values_read') is not False or data.get('app_pid')!=pid
            or not isinstance(nodes,list) or not 1<=len(nodes)<=512
            or any(not isinstance(n,dict) or not isinstance(n.get('path'),list)
                or any(type(i) is not int or i<0 for i in n['path']) for n in nodes)
            or len({tuple(n['path']) for n in nodes})!=len(nodes)):
        raise ValueError('complete_unique_public_App_metadata_required')
    visible=[n for n in nodes if n.get('showing')]
    windows=[n for n in visible if n.get('role')==23]
    if (len(windows)!=1 or windows[0].get('path')!=[0]
        or windows[0].get('bounds')!={'x':0,'y':0,'width':1280,'height':900}
        or not all(windows[0].get(k) is True for k in ('enabled','sensitive'))):
        raise ValueError('public_App_window_changed')
    if any(n.get('dialog') or n.get('modal') or n.get('file_chooser') for n in visible):
        raise ValueError('unknown_public_modal_no_input')
    return nodes


def _safe_target(node,role,bounds,button=False):
    if (node.get('role')!=role or node.get('bounds')!=bounds
            or any(node.get(k) is not True for k in ('showing','enabled','sensitive'))
            or any(node.get(k) is not False for k in ('modal','file_chooser','dialog','entry','editable','editable_text_interface'))
            or any(type(node.get(k)) is not bool for k in ('focused','focusable','selected','checked'))
            or any(node.get(k) is not False for k in ('selected','checked'))
            or (not button and (node.get('focusable') is not False or node.get('focused') is not False))
            or node.get('button') is not button or node.get('action_interface') is not button
            or (button and node.get('allowed_actions')!=['click'])
            or (not button and node.get('allowed_actions') is not None)):
        raise ValueError('controlled_target_public_safety_changed')


def saved_title(data,pid):
    nodes=complete_public(data,pid)
    matches=[n for n in nodes if n.get('showing') and n.get('label')==TITLE]
    if len(matches)!=1 or matches[0]['path']!=[0,9]:
        raise ValueError('exact_saved_alpha_title_not_unique')
    _safe_target(matches[0],29,{'x':138,'y':44,'width':675,'height':32})
    return matches[0]


def gallery_target(data,pid,contract):
    nodes=complete_public(data,pid)
    if len(nodes)!=contract['node_count']:
        raise ValueError('unknown_gallery_full_tree_count')
    closed={TITLE,'打开画布项目'+TITLE,'未命名画布','Untitled Canvas','打开画布项目未命名画布','打开画布项目Untitled Canvas'}
    projected=[]
    for n in nodes:
        item={k:n.get(k) for k in contract['keys']}
        if item.get('label') in closed: item['label']=None
        projected.append(item)
    digest=hashlib.sha256(json.dumps(projected,ensure_ascii=False,separators=(',',':')).encode()).hexdigest()
    if digest!=contract['sha256']:
        raise ValueError('unknown_gallery_full_tree_projection')
    titles=[n for n in nodes if n.get('showing') and n.get('label')==TITLE]
    buttons=[n for n in nodes if n.get('showing') and n.get('label')=='打开画布项目'+TITLE]
    if len(titles)!=1 or len(buttons)!=1:
        raise ValueError('fixed_alpha_card_or_title_not_unique')
    title,button=titles[0],buttons[0]
    pair={493:([0,16],[0,15]),874:([0,20],[0,19])}
    left=(button.get('bounds') or {}).get('x')
    if left not in pair or (title['path'],button['path'])!=pair[left]:
        raise ValueError('fixed_alpha_card_pair_or_slot_changed')
    _safe_target(title,29,{'x':left,'y':366,'width':357,'height':14})
    _safe_target(button,43,{'x':left,'y':144,'width':357,'height':214},True)
    return button


def validate_public_capture(data, app_pid):
    """Complete public App metadata must exclude Settings before saved pixels."""
    nodes = data.get('nodes')
    if (data.get('status') != 'public_metadata_observed' or data.get('coverage_complete') is not True
            or data.get('field_values_read') is not False or data.get('app_pid') != app_pid
            or not isinstance(nodes, list) or not 1 <= len(nodes) <= 512
            or any(not isinstance(n, dict) or not isinstance(n.get('path'), list)
                   or any(type(i) is not int or i < 0 for i in n['path']) for n in nodes)):
        raise ValueError('complete_public_App_metadata_required_before_pixels')
    if len({tuple(n['path']) for n in nodes}) != len(nodes):
        raise ValueError('duplicate_public_App_path_before_pixels')
    visible = [n for n in nodes if n.get('showing')]
    settings_bounds = ({'x': 596, 'y': 147, 'width': 188, 'height': 36},
                       {'x': 596, 'y': 222, 'width': 188, 'height': 36},
                       {'x': 144, 'y': 68, 'width': 48, 'height': 24},
                       {'x': 1216, 'y': 68, 'width': 26, 'height': 128})
    setting_labels = ('工作模式', '外观', '主题', '通用', '常规', 'General', 'Appearance',
                      'Theme', 'Work mode', '本地 Agent 权限', 'Local Agent Permissions')
    if any(n.get('label') in setting_labels
           or (isinstance(n.get('label'), str) and any(n['label'].startswith(label + separator)
               for label in ('工作模式', '外观', 'Work mode', 'Appearance')
               for separator in ('，', ', ', ': ', '：')))
           or n.get('bounds') in settings_bounds for n in visible):
        raise ValueError('Settings_context_forbids_saved_pixels')
    windows = [n for n in visible if n.get('role') == 23]
    if (len(windows) != 1 or windows[0].get('path') != [0]
            or windows[0].get('bounds') != {'x': 0, 'y': 0, 'width': 1280, 'height': 900}
            or not all(windows[0].get(k) is True for k in ('enabled', 'sensitive'))):
        raise ValueError('public_App_window_changed_before_pixels')
    if any(n.get('dialog') or n.get('modal') or n.get('file_chooser') for n in visible):
        raise ValueError('unknown_App_modal_forbids_saved_pixels')
    return True



def read_owned(path, maximum=2 * 1024 * 1024):
    info = path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
            or info.st_uid != os.getuid() or info.st_size > maximum):
        raise ValueError('bounded_owned_regular_state_file_required')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        actual = os.fstat(stream.fileno())
        if (actual.st_dev, actual.st_ino, actual.st_size, actual.st_mtime_ns) != (
                info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns):
            raise ValueError('state_file_changed_before_read')
        data = stream.read(maximum + 1)
        after = os.fstat(stream.fileno())
    final = path.lstat()
    identity = lambda i: (i.st_dev, i.st_ino, i.st_size, i.st_mtime_ns)
    if len(data) != info.st_size or identity(after) != identity(info) or identity(final) != identity(info):
        raise ValueError('state_file_changed_during_read')
    return data
