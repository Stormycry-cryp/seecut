"""GrsAI media contracts checked against public documentation on 2026-10-15.

Channel/quality variants retain distinct identities. Only the documented
fast/Lite alias is collapsed; this is not a provider fallback policy.
"""
from copy import deepcopy

ALIASES = {"nano-banana-fast": "nano-banana-2-lite"}
MAINTENANCE = frozenset({"gpt-image-2.5-flare", "gpt-image-2.5-sunburst"})
RATIOS = ['auto', '1:1', '16:9', '9:16', '4:3', '3:4', '3:2', '2:3', '5:4', '4:5', '21:9']
PIXELS = ['auto', '1024x1024', '2048x2048', '2880x2880', '1280x720', '2048x1152', '3840x2160',
          '720x1280', '1152x2048', '2160x3840', '1152x864', '2304x1728', '3264x2448',
          '864x1152', '1728x2304', '2448x3264', '1536x1024', '2048x1360', '3504x2336',
          '1024x1536', '1360x2048', '2336x3504', '1120x896', '2240x1792', '3200x2560',
          '896x1120', '1792x2240', '2560x3200', '1456x624', '2912x1248', '3840x1648',
          '624x1456', '1248x2912', '1648x3840', '688x2048', '1280x3840', '2048x688',
          '3840x1280', '1536x768', '3072x1536', '3840x1920', '768x1536', '1536x3072', '1920x3840']


def enum(values, default=None, *, integer=False):
    return dict(type='enum', required=True, values=values,
                **({'default': default} if default is not None else {}),
                **({'value_type': 'integer'} if integer else {}))


def model(identity, kind, parameters):
    references = dict(type='asset_list', required=False, min_items=0,
                      max_items=16 if kind == 'image' else 12,
                      accepted_media=['image/*'] if kind == 'image' else ['image/*', 'audio/*'],
                      max_per_kind={'image': 16, 'audio': 0, 'video': 0} if kind == 'image' else {'image': 9, 'audio': 3, 'video': 0})
    return dict(id=identity, canonical_id=identity, capability_version='grsai-media-2026-10-15',
                kind=kind, provider='grsai', display_name=identity,
                operations=['generate', 'edit'] if kind == 'image' else ['generate'],
                parameters=dict(quantity=enum([1, 2, 3, 4], 1, integer=True),
                                prompt=dict(type='string', required=True, max_length=12000),
                                reference_asset_ids=references, **parameters),
                fixed_parameters={}, billing_dimensions=['operation', 'size'] if kind == 'image' else ['resolution', 'duration'])


def catalog():
    rows = []
    for identity, sizes in [
        ('nano-banana-2.1', ['1K', '2K', '4K']), ('nano-banana-pro', ['1K', '2K', '4K']),
        ('nano-banana-2', ['1K', '2K', '4K']), ('nano-banana-2-lite', ['auto']),
        ('nano-banana-pro-cl', ['1K']), ('nano-banana-2-cl', ['1K']),
        ('nano-banana-2-2k-cl', ['2K']), ('nano-banana-pro-vip', ['1K', '2K']),
        ('nano-banana-pro-4k-vip', ['4K']), ('nano-banana-2-4k-cl', ['4K']),
    ]:
        ratios = RATIOS + (['1:4', '4:1', '1:8', '8:1'] if identity in {'nano-banana-2', 'nano-banana-2-cl', 'nano-banana-2-2k-cl', 'nano-banana-2-4k-cl'} else [])
        rows.append(model(identity, 'image', dict(size=enum(sizes, sizes[0]), aspect_ratio=enum(ratios, 'auto'))))
    for identity in ['gpt-image-2', 'gpt-image-2.5', 'gpt-image-2-vip']:
        sizes = PIXELS if identity.endswith('-vip') else ['auto', '1024x1024', '1672x941', '941x1672', '1443x1090', '1090x1443', '1536x1024', '1024x1536', '1408x1120', '1120x1408', '1920x832', '832x1920', '896x1792', '1792x896']
        rows.append(model(identity, 'image', dict(size=enum(sizes, 'auto'), quality=enum(['medium'] if identity.endswith('-vip') else ['auto'], 'medium' if identity.endswith('-vip') else 'auto'))))
    mini = model('minimax-h3', 'video', dict(resolution=enum(['480p', '768p', '1080p'], '480p'),
                 duration=enum(list(range(1, 16)), 5, integer=True), aspect_ratio=enum(['16:9', '9:16'], '16:9')))
    mini['duration_resolution_map'] = [dict(resolution=['480p', '768p'], duration=list(range(1, 16))), dict(resolution=['1080p'], duration=list(range(1, 11)))]
    rows.append(mini)
    rows.append(model('agnes-video-2.5', 'video', dict(resolution=enum(['720p', '1080p', '2k'], '720p'),
                      duration=enum(list(range(4, 13)), 5, integer=True), aspect_ratio=enum(['16:9', '9:16', '21:9', '4:3', '1:1', '3:4'], '16:9'))))
    return deepcopy(rows)


def validate_combination(model, request):
    from .errors import ApiError
    if model['provider'] != 'grsai':
        return
    if request['model'] == 'minimax-h3' and request.get('resolution') == '1080p' and request.get('duration', 0) > 10:
        raise ApiError(422, 'INVALID_PARAMETER', '1080p 视频最长 10 秒', {'parameter': 'duration'})
    if request.get('operation') == 'edit' and not request.get('reference_asset_ids'):
        raise ApiError(422, 'MISSING_PARAMETER', '图片编辑需要参考图片')
