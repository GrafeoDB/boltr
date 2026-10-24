# /// script
# requires-python = ">=3.12"
# dependencies = ["neo4j==6.1.0"]
# ///
"""Several open results in one explicit transaction, read with the official
neo4j Python driver against the `driver_server` example.

The driver pulls 1000 records per PULL, so every result here needs further
PULL (and DISCARD) messages addressed by `qid` while a later result is open.

Run from anywhere:

    uv run tests/drivers/python/multiple_results.py
"""

import subprocess
import sys
from pathlib import Path

from neo4j import GraphDatabase

REPO = Path(__file__).resolve().parents[3]


def start_server() -> tuple[subprocess.Popen[str], str]:
    server = subprocess.Popen(
        ["cargo", "run", "--quiet", "--example", "driver_server", "--", "127.0.0.1:0"],
        cwd=REPO,
        stdout=subprocess.PIPE,
        text=True,
    )
    line = server.stdout.readline() if server.stdout else ""
    if not line.startswith("listening on "):
        server.kill()
        sys.exit(f"driver_server did not start: {line!r}")
    return server, line.removeprefix("listening on ").strip()


def interleaved_results_commit(driver) -> None:
    with driver.session() as session:
        with session.begin_transaction() as tx:
            first = tx.run("ROWS 1500")
            second = tx.run("ROWS 1200")
            head = [next(first)["i"] for _ in range(1100)]
            assert [record["i"] for record in second] == list(range(1200))
            assert head + [record["i"] for record in first] == list(range(1500))
            third = tx.run("ROWS 2500")
            assert next(third)["i"] == 0
            third.consume()
            tx.commit()
        assert session.last_bookmarks(), "commit returned no bookmark"


def rollback_with_open_results(driver) -> None:
    with driver.session() as session:
        tx = session.begin_transaction()
        first = tx.run("ROWS 1500")
        tx.run("ROWS 1200")
        assert next(first)["i"] == 0
        tx.rollback()
        assert session.run("ROWS 3").value() == [0, 1, 2]


def main() -> None:
    server, address = start_server()
    try:
        with GraphDatabase.driver(f"bolt://{address}", auth=("user", "password")) as driver:
            for test in (interleaved_results_commit, rollback_with_open_results):
                test(driver)
                print(f"ok {test.__name__}")
    finally:
        server.kill()
        server.wait()


if __name__ == "__main__":
    main()
