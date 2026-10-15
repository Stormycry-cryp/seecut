"""Pixel checks for the six-color QA-owned fixture; no App access or UI actions."""
import hashlib

PALETTE = ((240, 32, 32), (32, 208, 64), (32, 64, 240),
           (240, 208, 32), (255, 0, 255), (0, 255, 255))


def locate_fixture(rgb, width, height, region=None):
    if len(rgb) != width * height * 3:
        raise ValueError('unexpected_RGB_size')
    left, top, right, bottom = region or (0, 0, width, height)
    lookup = {bytes(color): index for index, color in enumerate(PALETTE)}
    groups = [[] for _ in PALETTE]
    for y in range(top, bottom):
        for x in range(left, right):
            offset = (y * width + x) * 3
            index = lookup.get(rgb[offset:offset + 3])
            if index is not None:
                groups[index].append((x, y))
    if any(len(points) < 8 for points in groups[:4]) or any(not points for points in groups[4:]):
        raise ValueError('owned_fixture_not_visibly_identified')
    for points in groups[:4]:
        area = ((max(x for x, _ in points) - min(x for x, _ in points) + 1)
                * (max(y for _, y in points) - min(y for _, y in points) + 1))
        if len(points) / area < 0.8:
            raise ValueError('multiple_or_occluded_fixture_regions')
    centroids = [(sum(x for x, _ in points) / len(points),
                  sum(y for _, y in points) / len(points)) for points in groups]
    red, green, blue, yellow, magenta, cyan = centroids
    if not (red[0] < green[0] and blue[0] < yellow[0] and red[1] < blue[1]
            and green[1] < yellow[1] and magenta[0] < cyan[0] and magenta[1] < cyan[1]):
        raise ValueError('fixture_palette_spatial_order_not_identified')
    points = [point for group in groups for point in group]
    bounds = [min(x for x, _ in points), min(y for _, y in points),
              max(x for x, _ in points) + 1, max(y for _, y in points) + 1]
    w, h = bounds[2] - bounds[0], bounds[3] - bounds[1]
    if not (8 <= w <= width and 6 <= h <= height and abs(w / h - 4 / 3) < 0.08):
        raise ValueError('fixture_bounds_or_aspect_not_identified')
    return {'bounds': bounds, 'centroids': centroids, 'color_pixels': [len(group) for group in groups],
            'color_bounds': [[min(x for x, _ in g), min(y for _, y in g),
                              max(x for x, _ in g) + 1, max(y for _, y in g) + 1] for g in groups],
            'RGB_sha256': hashlib.sha256(rgb).hexdigest()}


def translated(before, after, dx, dy):
    b, a = before['bounds'], after['bounds']
    return (all(a[index] - b[index] == (dx if index % 2 == 0 else dy) for index in range(4))
            and before['color_pixels'] == after['color_pixels'])


def translated_with_canvas_clip(before, after, dx, dy):
    """Current reviewed full-canvas fixture: clipping and fractional raster phase allowed.

    The canvas stays fixed. Both inner markers must move with the pointer; all
    six color rectangles must equal their translated, canvas-clipped rectangles
    within three display pixels. This tolerates edge interpolation, not a fixed
    image or changed scale. Zero-shift/restoration continues to use translated().
    """
    canvas = before['bounds']
    source, target = before['color_bounds'], after['color_bounds']
    if dx == 0 or dy == 0:
        return False
    for index in (4, 5):
        for axis, delta in ((0, dx), (1, dy)):
            shift = after['centroids'][index][axis] - before['centroids'][index][axis]
            if abs(shift - delta) > 2:
                return False
    for b, a in zip(source, target):
        expected = [max(canvas[0], b[0] + dx), max(canvas[1], b[1] + dy),
                    min(canvas[2], b[2] + dx), min(canvas[3], b[3] + dy)]
        if expected[0] >= expected[2] or expected[1] >= expected[3]:
            return False
        if any(abs(actual - wanted) > 3 for actual, wanted in zip(a, expected)):
            return False
    return True
