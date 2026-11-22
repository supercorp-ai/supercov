def statements(items, flag):
    result = []
    count: int = 0
    unset: int
    other = None
    a = b = 5
    first, second = items[0], items[1]
    for item in items:
        if item is None:
            continue
        if item > 10:
            break
        result.append(item)
    while flag:
        flag = False
    pick = (lambda x: x + 1) if flag else (lambda: None)
    choose = items[0] if flag or count else items[1]
    match items:
        case [one]:
            count = 1
        case [one, two]:
            count = 2
        # a comment before a case
        case _:
            count = 3
    match flag:
        case True:
            pass
    try:
        x = 1 / 0
    except ZeroDivisionError as error:
        x = 0
    finally:
        x += 1
    with open("f") as handle:
        handle.read()
    global GLOBAL
    GLOBAL = 2
    def inner(value=3, other=True, name="Inner", computed=abs(-1), *args, key=False, **kwargs):
        return value * 2
    class Local:
        attribute = 4

        def method(self):
            return 5
    del result[0]
    assert count >= 0, "Count must be positive"
    raise ValueError("Bad") from None


async def asynchronous(items):
    async for item in items:
        await item
    async with items as value:
        yield value * 2


def defaults(a, b=1, c="Text", d=True, e=None, f=-1, g=(1, 2), *, h=2.5, i=False):
    return a, b, c, d, e, f, g, h, i


LEVEL = 1 + 2


class Plain:
    attribute = 1 + 2
    single = "Text"

    def method(self, x=0):
        return x + 1

    class Nested:
        def inner(self):
            return 1 + 1


class OneLine: value = 1 + 2


if LEVEL:
    def conditional():
        return 1 + 2


def singletons(flag):
    match flag:
        case False:
            x = 1

        # a comment at the case's indentation
        case None:
            x = 2
            # a comment deeper than the next case
        case _:
            x = 3
    try:
        x += 1
    finally:
        x -= 1
    return x


def parenthesized_first(a, b):
    pair = (a), b
    return pair
