"""A bounded LRU cache with a per-entry TTL."""
import time


class LruCache:
    def __init__(self, capacity, ttl, clock=time.monotonic):
        self.capacity = capacity
        self.ttl = ttl
        self.clock = clock
        self._entries = {}

    def get(self, key):
        raise NotImplementedError("get")

    def put(self, key, value):
        raise NotImplementedError("put")

    def __len__(self):
        return len(self._entries)
