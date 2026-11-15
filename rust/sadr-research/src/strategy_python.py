# Trusted isolated worker. Model source is never run before validation and limits.
import ast
import ctypes
import json
import math
import resource
import sys
from types import MappingProxyType

request = json.loads(sys.stdin.buffer.read())
source = request['source']
tree = ast.parse(source, mode='exec')
allowed = (ast.Module, ast.FunctionDef, ast.arguments, ast.arg, ast.Return,
           ast.Assign, ast.AugAssign, ast.Expr, ast.If, ast.For, ast.While,
           ast.Break, ast.Continue, ast.Pass, ast.Name, ast.Load, ast.Store,
           ast.Constant, ast.List, ast.Tuple, ast.Dict, ast.Subscript, ast.Slice,
           ast.BinOp, ast.UnaryOp, ast.BoolOp, ast.Compare, ast.IfExp, ast.Call,
           ast.Add, ast.Sub, ast.Mult, ast.Div, ast.FloorDiv, ast.Mod, ast.Pow,
           ast.USub, ast.UAdd, ast.Not, ast.And, ast.Or, ast.Eq, ast.NotEq,
           ast.Lt, ast.LtE, ast.Gt, ast.GtE, ast.In, ast.NotIn)
functions = {'abs': abs, 'min': min, 'max': max, 'sum': sum, 'len': len,
             'range': range, 'sorted': sorted, 'enumerate': enumerate,
             'zip': zip, 'float': float, 'int': int,
             'sqrt': math.sqrt, 'log': math.log, 'exp': math.exp,
             'sin': math.sin, 'cos': math.cos, 'isfinite': math.isfinite}
names = set(functions)
for node in ast.walk(tree):
    if isinstance(node, ast.FunctionDef):
        names.add(node.name)
for node in ast.walk(tree):
    if not isinstance(node, allowed):
        raise ValueError('unsupported Python syntax')
    if isinstance(node, (ast.Name, ast.arg, ast.FunctionDef)):
        name = node.id if isinstance(node, ast.Name) else node.arg if isinstance(node, ast.arg) else node.name
        if name.startswith('_'):
            raise ValueError('private name')
    if isinstance(node, ast.Call) and (not isinstance(node.func, ast.Name) or node.func.id not in names or node.keywords):
        raise ValueError('unsupported call')
code = compile(tree, '<scorer>', 'exec')
def freeze(value):
    if isinstance(value, dict):
        return MappingProxyType({key: freeze(item) for key, item in value.items()})
    if isinstance(value, list):
        return tuple(freeze(item) for item in value)
    return value
snapshot = freeze(request['input'])
resource.setrlimit(resource.RLIMIT_AS, (request['memory'], request['memory']))
resource.setrlimit(resource.RLIMIT_CPU, (request['cpu'], request['cpu']))
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
# No imports, paths, processes, sockets, entropy or clock syscalls.
# Attribute access/imports are rejected above, including vDSO clock access.
libc = ctypes.CDLL(None, use_errno=True)
class Filter(ctypes.Structure):
    _fields_ = [('code', ctypes.c_ushort), ('jt', ctypes.c_ubyte), ('jf', ctypes.c_ubyte), ('k', ctypes.c_uint)]
class Program(ctypes.Structure):
    _fields_ = [('len', ctypes.c_ushort), ('filter', ctypes.POINTER(Filter))]
# x86_64 only: other hosts fail closed rather than installing the wrong policy.
if sys.platform != 'linux' or __import__('platform').machine() != 'x86_64':
    raise ValueError('unsupported sandbox host')
# Validate architecture before interpreting syscall numbers (including x32).
items = [Filter(0x20, 0, 0, 4), Filter(0x15, 1, 0, 0xc000003e), Filter(0x06, 0, 0, 0x80000000), Filter(0x20, 0, 0, 0)]
# syscalls that CPython may need after initialization. Resource-limit changes,
# descriptor reads, file operations and every process/network syscall stay denied.
for number in (3, 11, 12, 13, 14, 15, 24, 25, 28, 60, 131, 186, 202, 231):
    items.extend((Filter(0x15, 0, 1, number), Filter(0x06, 0, 0, 0x7fff0000)))
# write(1, ...): the only writable descriptor is the bounded output pipe.
items.extend((Filter(0x15, 0, 4, 1), Filter(0x20, 0, 0, 16),
              Filter(0x15, 0, 1, 1), Filter(0x06, 0, 0, 0x7fff0000),
              Filter(0x06, 0, 0, 0x80000000)))
# Anonymous, non-executable mmap only. Inherited descriptors cannot be mapped.
items.extend((Filter(0x15, 0, 8, 9), Filter(0x20, 0, 0, 40),
              Filter(0x45, 0, 5, 0x20), Filter(0x20, 0, 0, 32),
              Filter(0x45, 3, 0, 4), Filter(0x20, 0, 0, 48),
              Filter(0x15, 0, 1, 0xffffffff), Filter(0x06, 0, 0, 0x7fff0000),
              Filter(0x06, 0, 0, 0x80000000)))
# mprotect cannot make existing memory executable either.
items.extend((Filter(0x15, 0, 4, 10), Filter(0x20, 0, 0, 32),
              Filter(0x45, 1, 0, 4), Filter(0x06, 0, 0, 0x7fff0000),
              Filter(0x06, 0, 0, 0x80000000)))
items.append(Filter(0x06, 0, 0, 0x80000000))
filters = (Filter * len(items))(*items)
program = Program(len(items), filters)
if libc.prctl(38, 1, 0, 0, 0) or libc.prctl(22, 2, ctypes.byref(program), 0, 0):
    raise ValueError('sandbox unavailable')
namespace = {'__builtins__': {}, **functions}
exec(code, namespace)
scores = namespace['score'](snapshot)
sys.stdout.write(json.dumps(scores, allow_nan=False, separators=(',', ':')))
sys.stdout.flush()
