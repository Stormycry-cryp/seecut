#!/usr/bin/env python3
"""从固定 Git 对象提取静态功能基线；不运行 CLI、GUI 或上游构建脚本。"""
import argparse
import json
from pathlib import Path
import re
import subprocess

from photocraft_source import ROOT, INTEGRATION, canonical, digest, load_pin, repository_identity, snapshot

STRING = r'"(?:[^"\\]|\\.)*"'


def strings(text):
    return [json.loads(s) for s in re.findall(STRING, text)]


def extract(source, pin):
    """提取的是固定上游，主题补丁不改变这一份保留功能的验收参照。"""
    upstream = {**pin, "patches": []}
    with canonical(source, upstream) as directory:
        def read(name):
            return (directory / name).read_text()

        files = snapshot(directory)
        state = read("crates/ui-egui/src/state.rs")
        tool_enum = state.split("pub enum Tool {", 1)[1].split("\n}", 1)[0]
        tools = re.findall(r"^    ([A-Za-z][A-Za-z0-9_]*),", tool_enum, flags=re.M)
        menu_path = "crates/ui-egui/src/menu_catalog.rs"
        menus = []
        menu_pattern = rf'^\s*\(&\[(.*?)\],\s*({STRING}),\s*(None|Some\({STRING}\)),\s*({STRING})\),'
        for match in re.finditer(menu_pattern, read(menu_path), flags=re.M):
            path, label, shortcut, identifier = match.groups()
            menus.append({"path": strings(path), "label": json.loads(label), "id": json.loads(identifier),
                          "shortcut": None if shortcut == "None" else strings(shortcut)[0]})
        shell_path = "crates/engine/src/shell_commands.rs"
        shell = []
        shell_pattern = rf'^\s*\(({STRING}),\s*({STRING}),\s*&\[(.*?)\],\s*(None|Some\({STRING}\))\),'
        for match in re.finditer(shell_pattern, read(shell_path), flags=re.M):
            identifier, label, path, shortcut = match.groups()
            shell.append({"id": json.loads(identifier), "label": json.loads(label), "path": strings(path),
                          "shortcut": None if shortcut == "None" else strings(shortcut)[0]})
        control_path = "crates/ui-egui/src/control.rs"
        methods = set()
        for line in read(control_path).splitlines():
            if re.match(r'^        "(?:ui|engine|jobs|app)\.', line) and "=>" in line:
                methods.update(strings(line.split("=>", 1)[0]))
        registry_path = "crates/engine/src/commands.rs"
        registry = read(registry_path)
        modules = re.findall(r"v\.extend\(crate::([a-z0-9_]+)::specs\(\)\)", registry)
        command_files = [registry_path]
        for module in modules:
            name = f"crates/engine/src/{module}.rs"
            if not (directory / name).is_file():
                name = f"crates/engine/src/{module}/mod.rs"
            command_files.append(name)
        commands = {}
        for name in command_files:
            text = read(name).split("#[cfg(test)]", 1)[0]
            # Literal macro ids and CommandSpec ids. Generated ids require compiled discovery.
            for match in re.finditer(rf'(?:\b(?:cmd|spec|[A-Za-z_]+_(?:cmd|spec))!\(\s*|\bid:\s*)({STRING})', text):
                identifier = json.loads(match.group(1))
                if re.fullmatch(r"[a-z][a-zA-Z0-9]*(?:\.[a-zA-Z0-9_]+)+", identifier):
                    commands.setdefault(identifier, set()).add(name)
            if name.endswith("/layer_style.rs"):
                for kind in re.findall(r'style_cmd!\(\s*"([A-Za-z]+)"', text):
                    commands.setdefault("layer.layerStyle." + kind, set()).add(name)
        hashes = {name: files[name]["sha256"] for name in sorted(set(command_files + [menu_path, shell_path, control_path,
                    "crates/ui-egui/src/state.rs", "crates/codecs/README.md", "crates/text/build.rs", "NOTICE", "ATTRIBUTION.md",
                    "LICENSE-MIT", "LICENSE-APACHE", "docs/brand/LICENSE-brand.txt"]))}
        codec_rows = [line for line in read("crates/codecs/README.md").splitlines() if line.startswith("| ")]
        if len(tools) != 67 or not menus or not shell or not methods or not commands:
            raise ValueError("上游功能结构变化，静态提取器需要审查")
        return {"schemaVersion": 1, "upstream": {k: pin[k] for k in ("repository", "commit", "version", "minimumRust")},
                "evidenceLevel": "static-source-only", "upstreamFileCount": len(files),
                "upstreamTreeSha256": digest(json.dumps(files, sort_keys=True, separators=(",", ":")).encode()),
                "extractionLimits": ["菜单目录包含禁用占位项，目录存在不证明对应功能可执行。",
                    "engineLiteralCandidates 为注册模块中静态字面量与已识别图层样式宏，不等同编译后的完整命令注册表；动态生成命令以 CLI commands --json 为准。",
                    "工具枚举、Shell 命令和控制方法存在不证明交互、文件保真或 GUI 当前 Session 已验收。"],
                "tools": tools, "menus": menus, "shellCommands": shell,
                "panels": [item for item in shell if item["id"].startswith("window.toggle.")],
                "uiControlMethods": sorted(methods), "engineRegistryModules": modules,
                "engineLiteralCandidates": [{"id": key, "sources": sorted(value)} for key, value in sorted(commands.items())],
                "codecCapabilityTableMarkdown": codec_rows,
                "buildRequirements": {"packages": [pin["desktopPackage"], pin["cliPackage"]], "features": pin["features"],
                    "heif": "官方构建启用；HEIF/HEIC只读，失败不得静默关闭。",
                    "fonts": "Inter/JetBrainsMono随源码；官方发行另含可选craft-fonts，未指定时使用系统CJK字体，发行前需核对字体与许可。",
                    "assets": "保留LICENSE-MIT、LICENSE-APACHE、NOTICE、ATTRIBUTION及第三方许可；ArtCraft品牌不在开源许可内。"},
                "sourceFileSha256": hashes,
                "runtimeEvidence": {"status": "not-run", "compiledCommandDump": None, "guiInteractions": "not-run",
                                    "fullFeatureAcceptance": "not-run", "sameSessionAgent": "not-integrated"}}


def compare_dump(path, baseline):
    commands = json.loads(path.read_text())
    if not isinstance(commands, list) or not commands or any(not isinstance(c, dict) or not isinstance(c.get("id"), str) for c in commands):
        raise ValueError("命令转储必须为实际 CLI commands --json 的非空数组")
    ids = [c["id"] for c in commands]
    if len(ids) != len(set(ids)):
        raise ValueError("命令转储包含重复 id")
    candidates = {c["id"] for c in baseline["engineLiteralCandidates"]}
    return {"evidenceLevel": "supplied-command-dump-only", "dumpSha256": digest(path.read_bytes()), "count": len(ids),
            "missingStaticCandidates": sorted(candidates - set(ids)), "additionalCompiledIds": sorted(set(ids) - candidates),
            "runtimeFeatureAcceptance": "not-proven", "provenance": "需另核对构建provenance和转储生成记录"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT / "external/photocraft")
    parser.add_argument("--output", type=Path, default=INTEGRATION / "feature-baseline.json")
    parser.add_argument("--check", action="store_true", help="只核对已保存的静态基线")
    parser.add_argument("--runtime-commands", type=Path, help="比较已实际生成的CLI命令转储，不执行CLI")
    args = parser.parse_args()
    pin = load_pin()
    repository_identity(args.source, pin, check_worktree=False)
    baseline = extract(args.source, pin)
    if args.check:
        if json.loads(args.output.read_text()) != baseline:
            raise ValueError("静态基线与固定上游不符")
        print("PhotoCraft 静态功能基线核对通过；运行时功能验收未执行")
    else:
        args.output.write_text(json.dumps(baseline, ensure_ascii=False, indent=2) + "\n")
        print(f"PhotoCraft 静态功能基线已生成：{args.output}")
    if args.runtime_commands:
        print(json.dumps(compare_dump(args.runtime_commands, baseline), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit(f"PhotoCraft 基线失败：{error}") from error
