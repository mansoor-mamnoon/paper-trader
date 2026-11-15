import unittest
from tuning_next import next_action


def report(deadline=180, selected=None):
    return {'timeout_seconds': deadline, 'selected_workers': selected,
            'rounds': [{'workers': n, 'samples': [{'correct': n == selected, 'error': None} for _ in range(4)]} for n in [1, 2, 4, 1]]}


class TuningContinuationTests(unittest.TestCase):
    def test_failed_measurements_advance_and_stop_at_the_longest_deadline(self):
        self.assertEqual(next_action(report(180)), {'deadline': 600})
        self.assertEqual(next_action(report(600)), {'deadline': 900})
        self.assertEqual(next_action(report(900)), {})

    def test_a_measured_worker_count_dispatches_the_replay(self):
        self.assertEqual(next_action(report(selected=2)), {'workers': 2})

    def test_an_incomplete_measurement_cannot_dispatch(self):
        partial = report()
        partial['rounds'].pop()
        with self.assertRaises(ValueError):
            next_action(partial)

    def test_the_latest_serial_observation_must_pass(self):
        data = report(selected=1)
        data['rounds'][-1]['samples'][0]['correct'] = False
        with self.assertRaises(ValueError):
            next_action(data)

    def test_a_provider_error_cannot_qualify_as_a_success(self):
        data = report(selected=4)
        data['rounds'][2]['samples'][0]['error'] = 'HTTP 504'
        with self.assertRaises(ValueError):
            next_action(data)
