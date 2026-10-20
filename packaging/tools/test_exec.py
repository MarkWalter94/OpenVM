"""Guest execution lifecycle tests. Run with Python 3 on Linux."""
import importlib.util
import importlib.machinery
import pathlib
import time
import unittest

source = str(pathlib.Path(__file__).with_name("TOOLS.PY"))
spec = importlib.util.spec_from_loader("guest_tools", importlib.machinery.SourceFileLoader("guest_tools", source))
tools = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tools)


class ExecutionTests(unittest.TestCase):
    def setUp(self):
        self.execution = tools.GuestExec()

    def tearDown(self):
        self.execution.close()

    def finish(self, ident=1):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            reply = self.execution.poll(ident)
            if not reply["running"]:
                return reply
            time.sleep(0.01)
        self.fail("execution did not finish")

    def test_output_and_exit_status(self):
        self.execution.start(1, "printf hello; printf problem >&2; exit 7", 3000)
        reply = self.finish()
        self.assertEqual((reply["stdout"], reply["stderr"], reply["exit_code"]), ("hello", "problem", 7))
        self.assertEqual(reply, self.execution.poll(1))

    def test_timeout_without_polling(self):
        self.execution.start(1, "sleep 30", 50)
        time.sleep(0.2)
        reply = self.execution.poll(1)
        self.assertFalse(reply["running"])
        self.assertTrue(reply["timed_out"])

    def test_cancel_and_busy(self):
        self.execution.start(1, "sleep 30", 30000)
        with self.assertRaises(ValueError):
            self.execution.start(2, "echo forbidden", 1000)
        self.execution.poll(1, cancel=True)
        self.assertTrue(self.finish()["cancelled"])
        with self.assertRaises(ValueError):
            self.execution.poll(2)

    def test_bounded_output_does_not_deadlock(self):
        self.execution.start(1, "head -c 1000000 /dev/zero; head -c 1000000 /dev/zero >&2", 3000)
        reply = self.finish()
        self.assertEqual(reply["exit_code"], 0)
        self.assertTrue(reply["truncated"])
        self.assertEqual(len(reply["stdout"]), 2048)
        self.assertEqual(len(reply["stderr"]), 2048)

    def test_close_terminates_job_and_next_start_replaces_result(self):
        self.execution.start(1, "sleep 30", 30000)
        self.execution.close()
        self.assertTrue(self.finish()["cancelled"])
        self.execution.start(2, "printf next", 1000)
        self.assertEqual(self.finish(2)["stdout"], "next")
        with self.assertRaises(ValueError):
            self.execution.poll(1)

    def test_invalid_requests(self):
        for command, timeout in [("", 1), ("a\0b", 1), ("x" * 8193, 1), ("true", 0), ("true", 120001)]:
            with self.assertRaises(ValueError):
                self.execution.start(1, command, timeout)


if __name__ == "__main__":
    unittest.main()
