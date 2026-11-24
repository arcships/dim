# Rate limiter notes

Requirements:

- Bursts up to 20 requests must be allowed after an idle period.
- Sustained traffic must stay at 10 requests per second.
- A rejected request must not consume capacity.

Decision: (not written yet)
