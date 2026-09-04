# PicoVolt Python starter

Requires Python 3.9 or newer. The exact PicoVolt wheel version is pinned in
`requirements.txt` so a copied starter never falls back to a source checkout.
The example commits a row, closes the database, and reopens it to demonstrate
durability. It is safe to run repeatedly.

```sh
python -m venv .venv
.venv/bin/python -m pip install -r requirements.txt
.venv/bin/python app.py
```

On Windows, replace `.venv/bin/python` with `.venv\Scripts\python.exe`.
