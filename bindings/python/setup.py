"""Force a platform-specific wheel.

The package is a ctypes binding that bundles a prebuilt shared library, so its
wheel must carry a platform tag (not the pure-Python ``py3-none-any`` tag).
Declaring that the distribution has compiled components does that. All other
metadata lives in pyproject.toml.
"""

import os

from setuptools import setup
from setuptools.command.bdist_wheel import bdist_wheel
from setuptools.dist import Distribution


class BinaryDistribution(Distribution):
    def has_ext_modules(self):
        return True


class PlatformWheel(bdist_wheel):
    """The bundled C library is platform-specific but independent of Python's ABI."""

    def finalize_options(self):
        super().finalize_options()
        self.root_is_pure = False

    def get_tag(self):
        # Release CI builds a true two-architecture dylib and explicitly asks
        # for the universal2 tag. Local single-architecture builds retain the
        # platform detected by setuptools.
        requested_platform = os.environ.get("PICOVOLT_WHEEL_PLATFORM")
        platform = (
            (requested_platform or self.plat_name)
            .replace("-", "_")
            .replace(".", "_")
        )
        return "py3", "none", platform


setup(distclass=BinaryDistribution, cmdclass={"bdist_wheel": PlatformWheel})
