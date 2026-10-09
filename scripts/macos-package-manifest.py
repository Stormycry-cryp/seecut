#!/usr/bin/env python3
"""Finalize one fresh runner-owned Seecut arm64 bundle; never install or launch it."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import subprocess
import zipfile

TARGET = 'aarch64-apple-darwin'
LICENSES = ('LICENSE', 'LICENSE-EXCEPTIONS.md', 'THIRD_PARTY_NOTICES.md')
FONT_LICENSE_SOURCE = 'src/crates/concat/ui/fonts/LICENSE-Synonym.txt'
FONT_LICENSE_BUNDLE = 'Contents/Resources/licenses/LICENSE-Synonym.txt'
MAX_BUNDLE_BYTES = 4 * 1024**3
MAX_ZIP_BYTES = 2 * 1024**3
MAX_MANIFEST_BYTES = 8 * 1024**2


def validate_dispatch(expected, actual, event, ref, default_branch, native_requested):
    if not re.fullmatch('[a-f0-9]{40}', expected) or actual != expected:
        raise ValueError('exact_source_sha_required')
    if (not default_branch or '\n' in default_branch or event != 'workflow_dispatch'
            or not ref.startswith('refs/heads/') or '\n' in ref
            or ref[len('refs/heads/'):] in (default_branch, 'main', 'master', 'gray')
            or native_requested):
        raise ValueError('isolated_feature_branch_dispatch_required')


def source_identity(args):
    root = args.repository
    if not root.is_absolute() or root.is_symlink() or not root.is_dir():
        raise ValueError('explicit_repository_required')
    actual = command(['git', 'rev-parse', 'HEAD'], root).strip()
    validate_dispatch(args.expected_sha, actual, os.environ.get('GITHUB_EVENT_NAME', ''),
                      os.environ.get('GITHUB_REF', ''), os.environ.get('SEECUT_DEFAULT_BRANCH', ''),
                      os.environ.get('SEECUT_NATIVE_REQUESTED', 'true') != 'false')
    if (os.environ.get('GITHUB_SHA') != actual
            or os.environ.get('GITHUB_WORKSPACE') != str(root)
            or os.environ.get('GITHUB_REPOSITORY') != 'Stormycry-cryp/seecut'
            or not re.fullmatch('[0-9]+', os.environ.get('GITHUB_RUN_ID', ''))
            or not re.fullmatch('[0-9]+', os.environ.get('GITHUB_RUN_ATTEMPT', ''))
            or os.environ.get('GITHUB_JOB') != 'macos_package_candidate'
            or command(['git', 'status', '--porcelain', '--untracked-files=no'], root).strip()
            or command(['uname', '-m']).strip() != 'arm64'):
        raise ValueError('exact_clean_source_and_arm64_runner_required')
    return actual


def command(argv, cwd=None):
    done = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, timeout=120)
    if done.returncode or len(done.stdout) + len(done.stderr) > 1024 * 1024:
        raise ValueError('required_packaging_command_failed:' + argv[0])
    return done.stdout + done.stderr


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return {'sha256': h.hexdigest(), 'bytes': path.stat().st_size}


def regular(path, maximum=MAX_BUNDLE_BYTES):
    if path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid():
        raise ValueError('owned_regular_file_required')
    if not 0 < path.stat().st_size <= maximum:
        raise ValueError('file_size_out_of_bounds')
    return path


def copy_bundled_font_license(root, app):
    """Preserve the repository's existing full text; no license/legal inference."""
    source = regular(root / FONT_LICENSE_SOURCE, 2 * 1024**2)
    target = app / FONT_LICENSE_BUNDLE
    if target.exists() or target.is_symlink():
        raise ValueError('fresh_font_license_destination_required')
    expected = digest(source)
    shutil.copyfile(source, target)
    actual = digest(regular(target, 2 * 1024**2))
    if actual != expected or digest(source) != expected:
        raise ValueError('complete_font_license_bytes_must_match_source')
    return {**actual, 'source_path': FONT_LICENSE_SOURCE,
            'bundle_path': FONT_LICENSE_BUNDLE,
            'distribution': 'inside_Seecut_macos_zip'}


def verify_bundled_font_license(root, app, record):
    expected = {key: record[key] for key in ('sha256', 'bytes')}
    if (digest(regular(root / FONT_LICENSE_SOURCE, 2 * 1024**2)) != expected
            or digest(regular(app / FONT_LICENSE_BUNDLE, 2 * 1024**2)) != expected):
        raise ValueError('complete_font_license_bytes_must_match_source')


def version(value):
    if not isinstance(value, str) or not re.fullmatch(r'[0-9]{1,3}(?:\.[0-9]{1,3}){0,2}', value):
        raise ValueError('explicit_macos_version_required')
    return tuple(int(x) for x in value.split('.')) + (0,) * (3 - len(value.split('.')))


def parse_load_commands(text):
    minimums, rpaths = [], []
    blocks = re.split(r'(?m)^Load command [0-9]+\s*$', text)
    for block in blocks:
        cmd = re.search(r'(?m)^\s*cmd (LC_[A-Z0-9_]+)\s*$', block)
        if not cmd:
            continue
        if cmd[1] == 'LC_BUILD_VERSION':
            platform = re.search(r'(?m)^\s*platform ([^\s]+)\s*$', block)
            minimum = re.search(r'(?m)^\s*minos ([^\s]+)\s*$', block)
            if not platform or platform[1] not in ('1', 'MACOS', 'macos') or not minimum:
                raise ValueError('actual_macos_platform_and_minimum_required')
            version(minimum[1])
            minimums.append(minimum[1])
        elif cmd[1] == 'LC_VERSION_MIN_MACOSX':
            minimum = re.search(r'(?m)^\s*version ([^\s]+)\s*$', block)
            if not minimum:
                raise ValueError('actual_macos_minimum_required')
            version(minimum[1])
            minimums.append(minimum[1])
        elif cmd[1].startswith('LC_VERSION_MIN_'):
            raise ValueError('non_macos_platform_forbidden')
        elif cmd[1] == 'LC_RPATH':
            path = re.search(r'(?m)^\s*path (.+) \(offset [0-9]+\)\s*$', block)
            if not path:
                raise ValueError('explicit_rpath_required')
            rpaths.append(path[1])
    if len(minimums) != 1:
        raise ValueError('exactly_one_actual_macos_minimum_required')
    return {'minimum_macos': minimums[0], 'rpaths': rpaths}


def parse_dependencies(text):
    lines = text.splitlines()
    if not lines or not lines[0].endswith(':'):
        raise ValueError('otool_dependency_header_required')
    result = []
    for line in lines[1:]:
        match = re.fullmatch(r'\s+(.+) \(compatibility version [^,]+, current version [^)]+\)', line)
        if not match:
            raise ValueError('unparsed_macho_dependency')
        result.append(match[1])
    return result


def expand_loader(value, image, executable, app):
    if value.startswith('@loader_path/'):
        path = image.parent / value[len('@loader_path/'):]
    elif value.startswith('@executable_path/'):
        path = executable.parent / value[len('@executable_path/'):]
    else:
        raise ValueError('relative_bundle_loader_path_required')
    resolved = path.resolve()
    if not resolved.is_relative_to(app.resolve()):
        raise ValueError('loader_path_outside_candidate')
    return resolved


def dependency_edges(image, record, records, executable, app):
    edges = []
    # Only existing bundle-relative rpaths may resolve non-system dependencies.
    roots = {expand_loader(p, image, executable, app) for p in record['rpaths']}
    roots.update(expand_loader(p, executable, executable, app) for p in records[executable]['rpaths'])
    for dep in record['install_names']:
        if image != executable and dep == '@rpath/' + image.name:
            edges.append({'install_name': dep, 'kind': 'dylib_id', 'target': image.relative_to(app).as_posix()})
            continue
        if dep.startswith('/usr/lib/') or dep.startswith('/System/Library/'):
            edges.append({'install_name': dep, 'kind': 'system'})
            continue
        if dep.startswith('@rpath/'):
            suffix = dep[len('@rpath/'):]
            if not suffix or '..' in PurePosixPath(suffix).parts or PurePosixPath(suffix).is_absolute():
                raise ValueError('bounded_bundle_rpath_required')
            targets = {(p / suffix).resolve() for p in roots if (p / suffix).is_file()}
            if len(targets) != 1:
                raise ValueError('one_bundled_rpath_target_required')
            target = targets.pop()
        elif dep.startswith(('@loader_path/', '@executable_path/')):
            target = expand_loader(dep, image, executable, app)
        else:
            raise ValueError('host_or_unknown_dependency_forbidden')
        if target not in records or not target.is_relative_to(app / 'Contents/Frameworks'):
            raise ValueError('dependency_must_be_verified_bundled_macho')
        edges.append({'install_name': dep, 'kind': 'bundled', 'target': target.relative_to(app).as_posix()})
    return edges


def bundle_files(app):
    if app.is_symlink() or not app.is_dir():
        raise ValueError('fresh_candidate_bundle_required')
    files = []
    for p in sorted(app.rglob('*')):
        if p.is_symlink() or not (p.is_dir() or p.is_file()):
            raise ValueError('bundle_symlink_or_special_file_forbidden')
        if p.is_file():
            regular(p)
            files.append(p)
    if not 1 <= len(files) <= 4096 or sum(p.stat().st_size for p in files) > MAX_BUNDLE_BYTES:
        raise ValueError('bundle_budget_exceeded')
    return files


def verify_zip(path, app):
    regular(path, MAX_ZIP_BYTES)
    expected = {p.relative_to(app.parent).as_posix(): digest(p) for p in bundle_files(app)}
    found, seen = {}, set()
    with zipfile.ZipFile(path) as archive:
        entries = archive.infolist()
        if len(entries) > 8192 or sum(i.file_size for i in entries) > MAX_BUNDLE_BYTES:
            raise ValueError('archive_budget_exceeded')
        for entry in entries:
            parts = PurePosixPath(entry.filename).parts
            if (entry.filename in seen or not parts or parts[0] != 'Seecut.app'
                    or '..' in parts or '\\' in entry.filename or '\x00' in entry.filename
                    or PurePosixPath(entry.filename).is_absolute()
                    or ((entry.external_attr >> 16) & 0o170000) == 0o120000):
                raise ValueError('exact_safe_bundle_archive_required')
            seen.add(entry.filename)
            if entry.is_dir():
                if not (app.parent / entry.filename).is_dir():
                    raise ValueError('unknown_archive_directory')
                continue
            if entry.filename not in expected:
                raise ValueError('unknown_archive_file')
            mode = (entry.external_attr >> 16) & 0o170000
            if mode not in (0, 0o100000):
                raise ValueError('regular_archive_file_required')
            if entry.filename.endswith('/Contents/MacOS/seecut') and not ((entry.external_attr >> 16) & 0o111):
                raise ValueError('archive_executable_permissions_required')
            h = hashlib.sha256()
            with archive.open(entry) as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b''):
                    h.update(block)
            found[entry.filename] = {'sha256': h.hexdigest(), 'bytes': entry.file_size}
    if found != expected:
        raise ValueError('complete_archive_bundle_bytes_must_match')
    return {'files': len(found), 'uncompressed_bytes': sum(v['bytes'] for v in found.values()),
            'complete_bundle_bytes_match': True}


def finalize(args):
    root, out, bare = args.repository, args.output, args.binary
    for p in (root, out, bare):
        if not p.is_absolute() or p.is_symlink():
            raise ValueError('explicit_absolute_paths_required')
    if out.stat().st_uid != os.getuid() or not out.is_dir() or root == out or out.is_relative_to(root):
        raise ValueError('runner_owned_output_outside_checkout_required')
    for name in ('candidate-manifest.json', 'SHA256SUMS', *LICENSES):
        if (out / name).exists() or (out / name).is_symlink():
            raise ValueError('fresh_output_files_required')
    actual = source_identity(args)
    runner_temp = Path(os.environ.get('RUNNER_TEMP', ''))
    target_dir = runner_temp / 'seecut-macos-arm64-target'
    if (not runner_temp.is_absolute() or not runner_temp.is_dir()
            or out != runner_temp / 'seecut-macos-arm64-candidate'
            or bare != target_dir / TARGET / 'app/concat'
            or os.environ.get('CARGO_TARGET_DIR') != str(target_dir)
            or os.environ.get('MACOSX_DEPLOYMENT_TARGET') != '12.0'
            or os.environ.get('GGML_NATIVE') != 'OFF'):
        raise ValueError('exact_runner_paths_and_build_environment_required')
    regular(bare)
    if command(['lipo', '-archs', str(bare)]).strip() != 'arm64':
        raise ValueError('actual_arm64_binary_required')
    bare_macho = parse_load_commands(command(['otool', '-l', str(bare)]))
    app = out / 'Seecut.app'
    bundle_files(app)
    app = app.resolve()
    executable = app / 'Contents/MacOS/seecut'
    regular(executable)
    images = [executable] + sorted((app / 'Contents/Frameworks').glob('*.dylib'))
    if not (app / 'Contents/Frameworks').is_dir():
        raise ValueError('actual_frameworks_directory_required')
    if any(p.is_file() and p not in images for p in (app / 'Contents/Frameworks').rglob('*')):
        raise ValueError('unexpected_framework_file')
    magics = {bytes.fromhex(v) for v in ('feedface', 'cefaedfe', 'feedfacf', 'cffaedfe',
                                       'cafebabe', 'bebafeca', 'cafebabf', 'bfbafeca')}
    for file in bundle_files(app):
        with file.open('rb') as stream:
            if stream.read(4) in magics and file not in images:
                raise ValueError('unexpected_unverified_bundle_macho')
    records = {}
    for image in images:
        regular(image)
        if command(['lipo', '-archs', str(image)]).strip() != 'arm64':
            raise ValueError('every_bundled_macho_must_be_arm64')
        record = parse_load_commands(command(['otool', '-l', str(image)]))
        record['install_names'] = parse_dependencies(command(['otool', '-L', str(image)]))
        records[image.resolve()] = record
    executable = executable.resolve()
    if version(bare_macho['minimum_macos']) != version(records[executable]['minimum_macos']):
        raise ValueError('bare_and_bundle_platform_minimum_must_match')
    for image, record in records.items():
        record['dependencies'] = dependency_edges(image, record, records, executable, app)
    plist_path = regular(app / 'Contents/Info.plist', 16384)
    info = plistlib.loads(plist_path.read_bytes())
    if any(info.get(k) != v for k, v in {
        'CFBundleName': 'Seecut', 'CFBundleDisplayName': 'Seecut',
        'CFBundleIdentifier': 'cloud.stormycry.seecut.preview', 'CFBundleExecutable': 'seecut',
    }.items()):
        raise ValueError('exact_Seecut_bundle_identity_required')
    original_minimum = info.get('LSMinimumSystemVersion')
    version(original_minimum)
    minimum = max([version('12.0')] + [version(r['minimum_macos']) for r in records.values()])
    minimum_text = '.'.join(str(v) for v in minimum)
    info['LSMinimumSystemVersion'] = minimum_text
    # Only the newly created runner bundle is changed, not product make-app.sh.
    plist_path.write_bytes(plistlib.dumps(info, sort_keys=False))
    licensedir = app / 'Contents/Resources/licenses'
    if licensedir.exists() or licensedir.is_symlink():
        raise ValueError('fresh_license_directory_required')
    licensedir.mkdir()
    for name in LICENSES:
        source = regular(root / name, 2 * 1024**2)
        shutil.copyfile(source, licensedir / name)
        shutil.copyfile(source, out / name)
    font_license = copy_bundled_font_license(root, app)
    command(['plutil', '-lint', str(plist_path)])
    for image in images[1:]:
        command(['codesign', '--force', '--sign', '-', str(image)])
    command(['codesign', '--force', '--deep', '--sign', '-', str(app)])
    command(['codesign', '--verify', '--deep', '--strict', '--verbose=2', str(app)])
    signature = command(['codesign', '--display', '--verbose=4', str(app)])
    if 'Signature=adhoc' not in signature or re.search(r'(?m)^Authority=', signature):
        raise ValueError('ad_hoc_candidate_signature_only')
    for image, record in records.items():
        command(['codesign', '--verify', '--strict', str(image)])
        record.update(digest(image))
    archive = regular(out / 'Seecut-macos.zip', MAX_ZIP_BYTES)
    # Replace only make-app's fresh runner ZIP after final bundle metadata/signing.
    archive.unlink()
    command(['ditto', '-c', '-k', '--keepParent', '--norsrc', '--noextattr', str(app), str(archive)])
    zip_check = verify_zip(archive, app)
    verify_bundled_font_license(root, app, font_license)
    logs = regular(out / 'build.log', 32 * 1024**2)
    sherpa = regular(args.sherpa_archive, MAX_ZIP_BYTES)
    if (not sherpa.is_absolute() or not sherpa.resolve().is_relative_to(runner_temp.resolve())
            or sherpa.name != 'sherpa.tar.bz2' or not sherpa.parent.name.startswith('seecut-sherpa.')):
        raise ValueError('runner_owned_sherpa_archive_required')
    if not re.fullmatch(r'https://github.com/k2-fsa/sherpa-onnx/releases/download/v[0-9.]+/sherpa-onnx-v[0-9.]+-osx-arm64-static-lib.tar.bz2', args.sherpa_url):
        raise ValueError('explicit_existing_sherpa_source_required')
    source_files = ['scripts/make-app.sh', 'scripts/generate-seecut-logo.sh',
                    '.github/workflows/ci.yml', '.github/workflows/build-app.yml',
                    'src/Cargo.lock', 'src/rust-toolchain.toml', *LICENSES, FONT_LICENSE_SOURCE]
    manifest = {
        'schema': 1, 'scope': 'macos-package-candidate', 'product': 'Seecut',
        'source_head': actual, 'expected_head': args.expected_sha,
        'repository_tree': command(['git', 'rev-parse', 'HEAD^{tree}'], root).strip(),
        'source_tree': command(['git', 'rev-parse', 'HEAD:src'], root).strip(),
        'assets_tree': command(['git', 'rev-parse', 'HEAD:assets'], root).strip(),
        'source_files': {n: digest(regular(root / n)) for n in source_files},
        'manifest_helper': digest(Path(__file__)),
        'runner': {'os': 'macOS', 'architecture': 'arm64', 'target': TARGET,
                   'os_version': command(['sw_vers', '-productVersion']).strip()},
        'build': {'profile': 'app', 'default_features': True, 'locked': True,
                  'declared_deployment_target': '12.0', 'ggml_native': 'OFF',
                  'rustc': command(['rustc', '-Vv'], root / 'src').strip(),
                  'cargo': command(['cargo', '-V'], root / 'src').strip(),
                  'compiler': command(['xcrun', 'clang', '--version']).strip(),
                  'rustflags': os.environ.get('RUSTFLAGS', '')},
        'dependencies': {'homebrew_formulae': json.loads(command(['brew', 'info', '--json=v2', 'ffmpeg', 'cmake', 'dylibbundler'])),
                         'sherpa_archive': {**digest(sherpa), 'url': args.sherpa_url}},
        'bare_binary': {**digest(bare), **bare_macho}, 'bundle_executable': digest(executable),
        'minimum_os': {'original_plist': original_minimum, 'final_plist': minimum_text,
                       'reason': 'max(declared build floor 12.0, every parsed bundled Mach-O minimum)',
                       'real_supported_os_versions': 'pending_actual_install_acceptance'},
        'macho': {p.relative_to(app).as_posix(): r for p, r in records.items()},
        'bundle_files': {p.relative_to(app).as_posix(): digest(p) for p in bundle_files(app)},
        'bundle_symlinks': [], 'zip': {**digest(archive), **zip_check},
        'signing': {'kind': 'ad-hoc', 'strict_verified': True, 'notarized': False},
        'license_material': {n: digest(out / n) for n in LICENSES},
        'bundled_font_license_material': {'LICENSE-Synonym.txt': font_license},
        'license_completeness_review': 'pending_main_distribution_review',
        'build_log': digest(logs),
        'github': {'repository': os.environ.get('GITHUB_REPOSITORY'),
                   'run_id': os.environ.get('GITHUB_RUN_ID'), 'run_attempt': os.environ.get('GITHUB_RUN_ATTEMPT'),
                   'job': os.environ.get('GITHUB_JOB'), 'ref': os.environ.get('GITHUB_REF'),
                   'artifact_id_and_upload_digest': 'supplied_by_actions_upload_and_checked_by_consumer'},
        'created_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'installation': 'not_performed', 'app_launch': 'not_performed', 'publication': 'not_performed',
    }
    destination = out / 'candidate-manifest.json'
    serialized = json.dumps(manifest, ensure_ascii=False, indent=2) + '\n'
    if len(serialized.encode()) > MAX_MANIFEST_BYTES:
        raise ValueError('manifest_budget_exceeded')
    with destination.open('x') as stream:
        stream.write(serialized)
    names = ['Seecut-macos.zip', 'candidate-manifest.json', 'build.log', *LICENSES]
    with (out / 'SHA256SUMS').open('x') as stream:
        for name in names:
            stream.write(digest(out / name)['sha256'] + '  ' + name + '\n')
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', type=Path, required=True)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--expected-sha', required=True)
    parser.add_argument('--sherpa-archive', type=Path)
    parser.add_argument('--sherpa-url')
    parser.add_argument('--verify-source-only', action='store_true')
    args = parser.parse_args()
    if args.verify_source_only:
        source_identity(args)
        print('exact_source_feature_branch_and_arm64_runner_verified')
        return
    if not all((args.output, args.binary, args.sherpa_archive, args.sherpa_url)):
        parser.error('finalize requires output, binary, sherpa-archive and sherpa-url')
    finalize(args)
    print('complete_candidate_manifest_and_SHA256SUMS_written; install/launch/publish not performed')


if __name__ == '__main__':
    main()
