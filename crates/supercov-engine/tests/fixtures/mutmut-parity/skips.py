import enum
import functools


@functools.cache
def decorated():
    return 1 + 2


@property
def prop(self):
    return 1 + 2


class Color(enum.Enum):
    RED = 1

    def __init__(self, value):
        self.code = value + 1

    def _generate_next_value_(name, start, count, last_values):
        return count + 1

    def describe(self):
        return self.code * 2


class Flags(enum.IntFlag):
    A = 1

    def check(self):
        return self.value + 1


class Special:
    def __getattribute__(self, name):
        return object.__getattribute__(self, name + "x")

    def __setattr__(self, name, value):
        object.__setattr__(self, name, value + 1)

    def __new__(cls):
        return object.__new__(cls) or 1 + 2

    def annotated(self, x: int = 1, y: "str" = "Y") -> "Return":
        z: "Local" = x + 1
        return z

    def with_inner_decorated(self):
        @functools.cache
        def helper():
            return 1 + 2

        return helper() + 3


def pragmas(a, b):
    x = a + b  # pragma: no mutate
    y = a - b
    if a > b:  # pragma: no mutate
        y = a * b
    if a < b:  # pragma: no mutate block
        y = a / b
        y = a % b
    # pragma: no mutate start
    z = a + 1
    w = b + 2
    # pragma: no mutate end
    v = (a +  # pragma: no mutate
         b)
    return x, y, z, w, v + 1


def block_pragma(a):
    first = a + 1
    # pragma: no mutate block
    second = a + 2
    third = a + 3
    return first, second, third
