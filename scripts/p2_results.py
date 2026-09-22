"""Keep interactive demo completion separate from unattended test assertions."""

NORMAL_DEMO_OUTCOMES = {'left', 'peer left', 'duration reached', 'interrupted'}


def describe_demo(results):
    """Return success/message without interpreting a user's stop as media failure.

    results contains (instance name, process exit code, optional final summary).
    Real process/session errors must still be reported, including missing evidence.
    """
    failures = []
    outcomes = []
    for name, exit_code, summary in results:
        if exit_code != 0:
            failures.append(f'{name}: process exited with code {exit_code}')
        elif summary is None:
            failures.append(f'{name}: no completed call result; inspect its log')
        elif summary.get('outcome') not in NORMAL_DEMO_OUTCOMES:
            failures.append(f"{name}: {summary.get('outcome', 'missing outcome')}")
        else:
            outcomes.append(f"{name}: {summary['outcome']} ({summary.get('elapsed_seconds', 0):.1f}s)")
    if failures:
        return False, 'Demo error: ' + '; '.join(failures)
    return True, ('Demo processes exited successfully — ' + '; '.join(outcomes)
                  + '. Interactive demo: full-duration performance checks were not run.')
