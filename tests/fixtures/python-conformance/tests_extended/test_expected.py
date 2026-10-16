import pytest

from app import shapes


@pytest.mark.xfail(raises=TypeError)
def test_expected_failure():
    shapes.chained("ten")


@pytest.mark.xfail(reason="passes unexpectedly, which pytest still counts as a pass")
def test_unexpected_pass():
    assert shapes.chained(5) == "small"


@pytest.mark.xfail(strict=True)
def test_strict_unexpected_pass():
    assert shapes.chained(50) == "large"


@pytest.fixture
def broken_setup():
    raise RuntimeError("setup")


def test_setup_error(broken_setup):
    assert shapes.chained(-1) == "negative"


@pytest.fixture
def broken_teardown():
    yield
    raise RuntimeError("teardown")


def test_teardown_error(broken_teardown):
    assert shapes.chained(1) == "small"
