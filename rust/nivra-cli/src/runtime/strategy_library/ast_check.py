import ast
import json
import sys

ALLOWED_IMPORTS = frozenset({
    "json",
    "math",
    "decimal",
    "statistics",
    "collections",
    "itertools",
    "functools",
    "datetime",
    "bisect",
    "heapq",
    "operator",
    "re",
    "typing",
    "dataclasses",
    "fractions",
    "random",
})

RESEARCH_ENV_IMPORTS = frozenset({
    "numpy",
    "pandas",
    "scipy",
    "statsmodels",
    "sklearn",
})

ALLOWED_SYS_ATTRS = frozenset({"argv", "stdin", "stdout"})

FORBIDDEN_BUILTINS = frozenset({
    "open",
    "exec",
    "eval",
    "compile",
    "__import__",
    "globals",
    "locals",
    "vars",
    "dir",
    "getattr",
    "setattr",
    "delattr",
    "hasattr",
    "breakpoint",
    "input",
    "memoryview",
    "__builtins__",
    "__loader__",
    "__spec__",
    "__file__",
    "__package__",
    "__cached__",
})


def has_format_traversal(pattern):
    depth = 0
    field = []
    i = 0
    n = len(pattern)
    while i < n:
        ch = pattern[i]
        if ch == "{":
            if i + 1 < n and pattern[i + 1] == "{":
                i += 2
                continue
            if depth == 0:
                field = []
            depth += 1
        elif ch == "}":
            if i + 1 < n and pattern[i + 1] == "}":
                i += 2
                continue
            if depth > 0:
                depth -= 1
                if depth == 0:
                    expr = "".join(field).split(":", 1)[0].split("!", 1)[0]
                    if "." in expr or "[" in expr:
                        return True
        elif depth > 0:
            field.append(ch)
        i += 1
    return False


class Checker(ast.NodeVisitor):
    def __init__(self, allowed_imports):
        self.allowed_imports = allowed_imports
        self.reasons = set()
        self._allowed_sys_names = set()
        self._allowed_type_names = set()

    def _reject(self, reason):
        self.reasons.add(reason)

    def check(self, tree):
        has_entry = False
        for node in tree.body:
            if isinstance(node, ast.FunctionDef) and node.name == "on_observation":
                positional = len(node.args.posonlyargs) + len(node.args.args)
                if positional >= 1 or node.args.vararg is not None:
                    has_entry = True
        if not has_entry:
            self._reject("missing_on_observation")
        self.visit(tree)
        return sorted(self.reasons)

    def visit_Import(self, node):
        for alias in node.names:
            top = alias.name.split(".", 1)[0]
            if alias.asname in FORBIDDEN_BUILTINS:
                self._reject(f"forbidden_builtin:{alias.asname}")
            if top == "sys":
                if alias.name != "sys" or alias.asname is not None:
                    self._reject("disallowed_sys_alias")
            elif top not in self.allowed_imports:
                self._reject(f"disallowed_import:{top}")

    def visit_ImportFrom(self, node):
        if node.level and node.level > 0:
            self._reject("disallowed_relative_import")
            return
        mod = node.module or ""
        top = mod.split(".", 1)[0]
        if any(alias.name == "*" for alias in node.names):
            self._reject(f"disallowed_star_import:{top or mod}")
        for alias in node.names:
            if alias.asname in FORBIDDEN_BUILTINS:
                self._reject(f"forbidden_builtin:{alias.asname}")
        if top == "sys":
            if mod != "sys":
                self._reject("disallowed_sys_submodule")
                return
            for alias in node.names:
                if alias.name not in ALLOWED_SYS_ATTRS:
                    self._reject(f"disallowed_sys_attr:{alias.name}")
                if alias.asname is not None:
                    self._reject("disallowed_sys_alias")
        elif top not in self.allowed_imports:
            self._reject(f"disallowed_import:{top}")

    def visit_FunctionDef(self, node):
        if node.name in FORBIDDEN_BUILTINS:
            self._reject(f"forbidden_builtin:{node.name}")
        for arg in node.args.posonlyargs + node.args.args + node.args.kwonlyargs:
            if arg.arg in FORBIDDEN_BUILTINS:
                self._reject(f"forbidden_builtin:{arg.arg}")
        if node.args.vararg and node.args.vararg.arg in FORBIDDEN_BUILTINS:
            self._reject(f"forbidden_builtin:{node.args.vararg.arg}")
        if node.args.kwarg and node.args.kwarg.arg in FORBIDDEN_BUILTINS:
            self._reject(f"forbidden_builtin:{node.args.kwarg.arg}")
        self.generic_visit(node)

    def visit_AsyncFunctionDef(self, node):
        self._reject("disallowed_async")

    def visit_Await(self, node):
        self._reject("disallowed_async")

    def visit_AsyncFor(self, node):
        self._reject("disallowed_async")

    def visit_AsyncWith(self, node):
        self._reject("disallowed_async")

    def visit_Call(self, node):
        if isinstance(node.func, ast.Name) and node.func.id == "type":
            if len(node.args) == 1 and not node.keywords:
                self._allowed_type_names.add(id(node.func))
            else:
                self._reject("forbidden_builtin:type_create")
        elif isinstance(node.func, ast.Attribute) and node.func.attr == "format":
            if not (
                isinstance(node.func.value, ast.Constant)
                and isinstance(node.func.value.value, str)
                and not has_format_traversal(node.func.value.value)
            ):
                self._reject("forbidden_format_attr")
        self.generic_visit(node)

    def visit_Attribute(self, node):
        if node.attr.startswith("__") and node.attr.endswith("__"):
            self._reject(f"forbidden_attribute:{node.attr}")
        elif node.attr == "format_map":
            self._reject("forbidden_attribute:format_map")
        if isinstance(node.value, ast.Name) and node.value.id == "sys":
            if node.attr in ALLOWED_SYS_ATTRS:
                self._allowed_sys_names.add(id(node.value))
            else:
                self._reject(f"disallowed_sys_attr:{node.attr}")
        self.generic_visit(node)

    def visit_Name(self, node):
        if node.id in FORBIDDEN_BUILTINS:
            self._reject(f"forbidden_builtin:{node.id}")
        elif node.id == "sys" and id(node) not in self._allowed_sys_names:
            self._reject("disallowed_sys_ref")
        elif node.id == "type" and id(node) not in self._allowed_type_names:
            self._reject("forbidden_builtin:type_ref")


def main():
    allow_env = "--allow-research-env" in sys.argv[1:]
    raw = sys.stdin.buffer.read()
    try:
        source = raw.decode("utf-8")
    except UnicodeDecodeError:
        print(json.dumps({"verdict": "unsafe", "reasons": ["invalid_utf8"]}))
        return
    if not source.strip() or "\x00" in source:
        print(json.dumps({"verdict": "unsafe", "reasons": ["empty_or_nul_source"]}))
        return
    try:
        tree = ast.parse(source, mode="exec")
    except SyntaxError:
        print(json.dumps({"verdict": "unsafe", "reasons": ["syntax_error"]}))
        return
    allowed = ALLOWED_IMPORTS | (RESEARCH_ENV_IMPORTS if allow_env else frozenset())
    reasons = Checker(allowed).check(tree)
    print(json.dumps({"verdict": "unsafe" if reasons else "safe", "reasons": reasons}))


if __name__ == "__main__":
    main()
