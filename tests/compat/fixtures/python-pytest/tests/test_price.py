from shop.price import price


def test_members_save_on_large_orders():
    assert price(200, True, False) == 180


def test_coupons_take_five_off():
    assert price(20, False, True) == 15
