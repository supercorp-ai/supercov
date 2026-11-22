def strings(name):
    plain = "hello"
    single = 'World'
    empty = ""
    escapes = "Tab\tNew\nHex\x41UniéBig\U0001F600Named\N{BULLET}End"
    raw = r"Raw\dPath\W+"
    raw_upper = R'A\Bc'
    prefixed = b"Bytes", u"Unicode", rb"RawBytes\X", Rb'Q'
    numbers = "123"
    doc = """Triple quoted"""
    other = '''Also triple'''
    joined = "Left" "Right"
    formatted = f"Value {name['Key']} and {name!r:>10}"
    mixed = "Plain" f"{name}"
    german = "Straße"
    return plain, single, empty, escapes, raw, raw_upper, prefixed, numbers, doc, other, joined, formatted, mixed, german


def docstring_only():
    "A one-line docstring in single quotes."
    return "Value"


def raw_escapes():
    short = r"Short\x4", r"Bad\xZZ", r"Bad\uZZZZ", r"Bad\UZZ", r"Name\Nx"
    ending = r"Ends\x41", "Tail\x41"
    return short, ending
