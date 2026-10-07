#!/usr/bin/env python3
from setuptools import setup
from setuptools_rust import Binding, RustExtension

setup(
    rust_extensions=[
        RustExtension(
            "janitor._common",
            "common-py/Cargo.toml",
            binding=Binding.PyO3,
            features=["extension-module"],
        ),
        RustExtension(
            "janitor._site",
            "site-py/Cargo.toml",
            binding=Binding.PyO3,
            features=["extension-module"],
        ),
    ]
)
