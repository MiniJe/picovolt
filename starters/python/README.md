# PicoVolt Python starter

Runs on Python 3.9 or newer; use a currently supported Python release for real
deployments because Python 3.9 itself is end-of-life. The exact PicoVolt wheel
version is pinned in `requirements.txt` so a copied starter never falls back to
a source checkout.
The example uses schema defaults and checks, commits a row, closes the database,
and reopens it to demonstrate durability. It is safe to run repeatedly.

```sh
python -m venv .venv
.venv/bin/python -m pip install -r requirements.txt
.venv/bin/python app.py
```

On Windows, replace `.venv/bin/python` with `.venv\Scripts\python.exe`.
