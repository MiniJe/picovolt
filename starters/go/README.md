# PicoVolt Go starter

The Go wrapper and its ABI header are downloaded from the public Go module
proxy. It uses cgo, so the matching native PicoVolt library must be available to
the compiler and dynamic loader. The automated release gate obtains that native
library from the matching PyPI wheel; it never reaches into the PicoVolt source
tree.

Release maintainers can exercise that exact clean-install path with:

```sh
python scripts/check_registry_starters.py run --starter go
```

Applications may instead ship the native library beside their executable or
install it in a platform library directory.
