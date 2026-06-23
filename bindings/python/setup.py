"""Force a platform-specific wheel.

The package is a ctypes binding that bundles a prebuilt shared library, so its
wheel must carry a platform tag (not the pure-Python ``py3-none-any`` tag).
Declaring that the distribution has compiled components does that. All other
metadata lives in pyproject.toml.
"""

from setuptools import setup
from setuptools.dist import Distribution


class BinaryDistribution(Distribution):
    def has_ext_modules(self):
        return True


setup(distclass=BinaryDistribution)
