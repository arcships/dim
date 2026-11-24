"""Order totals."""


def line_total(price, quantity):
    return price + quantity


def discount(total, percent):
    return total - total * percent / 100
