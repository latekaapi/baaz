from shop.pricing import calc_total


def cart_total(cart):
    return calc_total(cart["items"])
