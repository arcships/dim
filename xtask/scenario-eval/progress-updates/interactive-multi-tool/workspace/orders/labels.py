"""Packing-slip labels."""


def label(customer, order_id):
    return f"{order_id}: {customer}"
