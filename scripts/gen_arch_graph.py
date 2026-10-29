#!/usr/bin/env python3
"""Generate the BlackHornet internal module dependency graph (graphify).

Parses every ``*.py`` file under ``src/`` with :mod:`ast`, extracts internal
imports (``import src.*`` / ``from src.*`` / relative imports that cross
package boundaries), and emits a Markdown report with:

* a Mermaid ``graph LR`` of package-level dependencies,
* a package inventory (files, LOC, fan-in / fan-out),
* the loose ops-line modules at ``src/`` root,
* orphan detection (packages with no internal edges).

Stdlib-only, deterministic output (sorted). Regenerate after structural
changes and commit the result:

    python3 scripts/gen_arch_graph.py

Output: ``docs/02-arch/MODULE_GRAPH.md`` (override with ``--out``).
"""

from __future__ import annotations

import argparse
import ast
import sys
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SRC = REPO / "src"
DEFAULT_OUT = REPO / "docs" / "02-arch" / "MODULE_GRAPH.md"


def _node_of(module: str) -> str:
    """Map a dotted ``src.*`` module path to its package-level node name."""
    parts = module.split(".")
    if parts[0] != "src" or len(parts) == 1:
        return "src(root)"
    return parts[1]


def _resolve_relative(node: ast.AST, pkg: str) -> str | None:
    """Resolve ``from .x import y`` style imports to a package-level node."""
    if not isinstance(node, ast.ImportFrom) or node.level == 0:
        return None
    base = pkg.split(".")[:-node.level] if node.level <= pkg.count(".") + 1 else []
    if node.module:
        base.append(node.module)
    target = ".".join(["src", *base]) if not base or base[0] != "src" else ".".join(base)
    return _node_of(target)


def _internal_target(stmt: ast.stmt, pkg: str) -> str | None:
    """Return the internal package node an import statement points at, if any."""
    if isinstance(stmt, ast.Import):
        for alias in stmt.names:
            if alias.name.startswith("src"):
                return _node_of(alias.name)
        return None
    if isinstance(stmt, ast.ImportFrom):
        if stmt.level and stmt.level > 1:  # crosses package boundary upward
            rel = _resolve_relative(stmt, pkg)
            return rel
        if stmt.module and stmt.module.startswith("src"):
            return _node_of(stmt.module)
        return None
    return None


def scan() -> tuple[dict[str, list[str]], dict[str, set[str]], dict[str, set[str]]]:
    """Return (package -> files, fan_in, fan_out) maps for src/."""
    files_by_pkg: dict[str, list[str]] = defaultdict(list)
    fan_in: dict[str, set[str]] = defaultdict(set)
    fan_out: dict[str, set[str]] = defaultdict(set)

    for path in sorted(SRC.rglob("*.py")):
        rel = path.relative_to(SRC)
        pkg = rel.parts[0] if len(rel.parts) > 1 else "src(root)"
        files_by_pkg[pkg].append(str(rel))
        try:
            tree = ast.parse(path.read_text(encoding="utf-8", errors="replace"))
        except SyntaxError as exc:  # pragma: no cover - repo must stay parseable
            print(f"WARNING: syntax error in {path}: {exc}", file=sys.stderr)
            continue
        src_pkg = ".".join(rel.parts[:-1]) if len(rel.parts) > 1 else ""
        for stmt in ast.walk(tree):
            if not isinstance(stmt, (ast.Import, ast.ImportFrom)):
                continue
            target = _internal_target(stmt, "src." + src_pkg if src_pkg else "src")
            if target and target != pkg:
                fan_out[pkg].add(target)
                fan_in[target].add(pkg)
    return files_by_pkg, fan_in, fan_out


def loc_of(rel_files: list[str]) -> int:
    total = 0
    for rel in rel_files:
        p = SRC / rel
        try:
            total += len(p.read_text(encoding="utf-8", errors="replace").splitlines())
        except OSError:
            pass
    return total


def mermaid(nodes: list[str], fan_out: dict[str, set[str]]) -> str:
    def nid(n: str) -> str:
        return "n_" + "".join(c if c.isalnum() else "_" for c in n)

    lines = ["```mermaid", "graph LR"]
    for n in nodes:
        lines.append(f"    {nid(n)}[\"{n}\"]")
    edges = sorted({(src, dst) for src in fan_out for dst in fan_out[src] if src in nodes and dst in nodes})
    for src, dst in edges:
        lines.append(f"    {nid(src)} --> {nid(dst)}")
    lines.append("```")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT, help="output markdown path")
    args = ap.parse_args()

    files_by_pkg, fan_in, fan_out = scan()
    loose = files_by_pkg.pop("src(root)", [])
    packages = sorted(files_by_pkg)
    nodes = packages + (["src(root)"] if loose else [])

    edges_total = len({(s, d) for s in fan_out for d in fan_out[s]})

    lines: list[str] = [
        "# BlackHornet — Module Dependency Graph",
        "",
        "> **GENERATED FILE** — do not edit by hand.",
        "> Regenerate with `python3 scripts/gen_arch_graph.py` and commit the result.",
        "> Graph granularity is package-level (direct children of `src/`); only",
        "> *internal* `src.*` imports are drawn, standard-library and third-party",
        "> imports are excluded. Deterministic (sorted) output.",
        "",
        f"- Packages: **{len(packages)}** + {len(loose)} loose ops-line module(s) at `src/` root",
        f"- Internal cross-package import edges: **{edges_total}**",
        f"- Total Python files under `src/`: **{sum(len(v) for v in files_by_pkg.values()) + len(loose)}**",
        "",
        "## 1. Package dependency graph",
        "",
        mermaid(nodes, fan_out),
        "",
        "## 2. Package inventory",
        "",
        "Sorted by fan-in (how many packages import it) — the load-bearing",
        "subsystems sit at the top.",
        "",
        "| Package | Files | LOC | Fan-in | Fan-out |",
        "|---|---:|---:|---:|---:|",
    ]
    rows = sorted(packages, key=lambda p: (-len(fan_in.get(p, ())), p))
    for p in rows:
        lines.append(
            f"| `{p}` | {len(files_by_pkg[p])} | {loc_of(files_by_pkg[p]):,} "
            f"| {len(fan_in.get(p, ()))} | {len(fan_out.get(p, ()))} |"
        )
    if loose:
        lines.append(
            f"| `src/` root (ops line) | {len(loose)} | {loc_of(loose):,} "
            f"| {len(fan_in.get('src(root)', ()))} | {len(fan_out.get('src(root)', ()))} |"
        )
    lines += ["", "## 3. Loose ops-line modules (`src/` root)", ""]
    if loose:
        lines += ["| Module | Purpose hint |", "|---|---|"]
        hints = {
            "colony_bridge.py": "colony bridge protocol exports",
            "exceptions.py": "shared exception types",
            "hermes_quant.py": "paper-trading main loop (L1–L5)",
            "immortal_daemon.py": "immortal-daemon ops shell",
            "memory_bridge.py": "BH bridge protocol v1 peer",
            "swarm_protocol.py": "swarm wire protocol",
            "upgrade_loop.py": "self-upgrade loop",
            "user_alignment.py": "user alignment checks",
            "watchdog.py": "service watchdog",
        }
        for m in sorted(loose):
            if m == "__init__.py":
                continue
            lines.append(f"| `src/{m}` | {hints.get(m, '—')} |")
    orphans = [p for p in packages if not fan_in.get(p) and not fan_out.get(p)]
    lines += [
        "",
        "## 4. Coupling notes",
        "",
        f"- Most depended-upon: "
        + (", ".join(f"`{name}` ({len(fan_in.get(name, ()))} inbound)" for name in rows[:3]))
        + ".",
        f"- Orphan packages (no internal edges): "
        + (", ".join(f"`{o}`" for o in orphans) if orphans else "none"),
        "",
    ]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {args.out} ({len(packages)} packages, {edges_total} edges, {len(loose)} loose)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
