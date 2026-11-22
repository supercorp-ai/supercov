def clauses(items):
    total = 0
    for item in items:  # pragma: no mutate
        total += item
    else:  # pragma: no mutate block
        total += 1
        total += 2
    while total > 100:  # pragma: no mutate
        total -= 1
    else:
        total *= 2
    try:  # pragma: no mutate block
        total = total / 3
    except ZeroDivisionError:  # pragma: no mutate
        total = 0
    except ValueError as error:
        total = -1
    else:
        total += 4
    finally:  # pragma: no mutate
        total += 5
    with open("f") as handle:  # pragma: no mutate
        total += 6
    if total:
        total += 7
    elif items:  # pragma: no mutate
        total += 8
    else:  # pragma: no mutate block
        total += 9
    return total


def matches(value):
    match value:  # pragma: no mutate
        case 1:
            return 1 + 1
        case 2:  # pragma: no mutate block
            return 2 + 2
        case _:
            return 3 + 3


def bracketed(a, b):
    result = call(
        a + 1,  # pragma: no mutate
        b - 1,
    )
    other = [
        a * 2,
        # pragma: no mutate
        b * 3,
    ]
    x = 1; y = 2  # pragma: no mutate
    return result, other, x, y


def ranges(a):
    # pragma: no mutate start
    first = a + 1
    second = a + 2
    # pragma: no mutate end
    third = a + 3
    if a:
        inner = a + 4
        # pragma: no mutate block
        more = a + 5
        last = a + 6
    after = a + 7
    return first, second, third, after


def single(a): return a + 1  # pragma: no mutate


def suite(a):
    if a: return a + 1  # pragma: no mutate
    return a - 1


class Blocked:  # pragma: no mutate block
    def method(self):
        return 1 + 1


class Open:
    def method(self):  # pragma: no mutate block
        return 1 + 1

    def other(self):
        return 2 + 2

    @staticmethod
    def static():
        return 3 + 3

    @classmethod
    def klass(cls):
        return 4 + 4


def dropped(a, b):
    return call(a,
                b)  # pragma: no mutate
