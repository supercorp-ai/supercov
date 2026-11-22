def price(amount, member, coupon):
    total = amount
    if member and amount > 100:
        total = total * 0.9
    if coupon:
        total = total - 5
    return max(total, 0)


def label(total):
    return "large" if total > 50 else "small"


def unused(values):
    for item in values:
        if item < 0:
            return False
    return True
