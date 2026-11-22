"""Operators of every kind mutmut swaps."""

import copy


def arithmetic(a, b):
    total = a + b - a * b / (b or 1)
    floor = a // b % 3 ** 2
    bits = a << 2 >> 1 & b | a ^ b
    sign = -a + +b - ~a
    return (total), floor, bits, sign, a @ b if False else None


def comparisons(a, b, c):
    first = a < b <= c > a >= b
    second = a == b != c
    third = a is b, a is not b, a in c, a not in c
    spaced = a<b and (a)  <  (b)
    return first and second or third and spaced or not a


def augmented(values):
    total = 0
    total += 1
    total -= 2
    total *= 3
    total /= 4
    total //= 5
    total %= 6
    total **= 2
    total <<= 1
    total >>= 1
    total &= 7
    total |= 8
    total ^= 9
    values[0]+=1
    return total


def chains(a, b, c, d):
    return a and b and c or d and not (a or b)


class Numbers:
    limit: int = 10

    def literals(self):
        return 0, 1, 1_000, 0x1F, 0o17, 0b11, 1.5, 1e3, 2.5e-5, 1e16, 1e20, 3j, 1.5j, 1e999, .5, 1., 123456789012345678901234567890

    @staticmethod
    def names():
        return True, False, None, copy.deepcopy, deepcopy

    @classmethod
    def unary(cls, flag):
        return not flag, ~flag, -flag, +flag, -(1)


def matrices(a, b, flag):
    a @= b
    product = a @ b
    picked = 1 if (a, b) else 2
    total = a + \
        b
    return product, picked, total
