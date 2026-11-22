def calls(text, items, mapping):
    first = text.lower().upper().strip()
    left = text.lstrip(), text.rstrip(), text.find("a"), text.rfind("a")
    just = text.ljust(5), text.rjust(5), text.index("x"), text.rindex("x")
    fixes = text.removeprefix("a"), text.removesuffix("b")
    parts = text.partition(":"), text.rpartition(":")
    split = text.split(), text.split(","), text.split(",", 1), text.rsplit(",", maxsplit=2), text.split(sep=",")
    created = dict(a=1, b=2), dict(items, c=3), dict(d=4, **mapping), dict()
    sized = len(items), isinstance(text, str), len(text.split(","))
    called = print(text, items, sep=None, end="!"), print(*items), print(None, text)
    nested = max(min(1, 2), 3)
    trailing = max(
        1,
        2,
    )
    generator = sum(x for x in items)
    return first, left, just, fixes, parts, split, created, sized, called, nested, trailing, generator
