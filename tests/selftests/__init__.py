# Package marker for the legacy selftests suite.
#
# Required so `python3 -m unittest discover -s tests/selftests -p "*_tests.py"
# -t <repo root>` can import this directory as tests.selftests (unittest
# discovery rejects non-package start dirs that differ from the top-level dir).
# pytest never collects anything here: no filename matches test_*.py/*_test.py.
