#include "../Sources/vibestudio/AppLockPolicy.h"
#include <cassert>
#include <iostream>

using Policy = VibeStudioLockPolicy;
using Result = Policy::AuthenticationResult;

static Policy authenticated() {
    Policy policy;
    assert(!policy.unlocked());
    policy.becomeActive();
    auto attempt = policy.beginAuthentication();
    assert(attempt != 0);
    assert(policy.finishAuthentication(attempt, true) == Result::unlocked);
    return policy;
}

int main() {
    // Active reading never expires, and a transient system interruption does
    // not start a background grace (including Face ID's own inactive cycle).
    auto reading = authenticated();
    assert(!reading.refresh(86400));
    reading.resignActive();
    assert(!reading.refresh(172800));
    reading.becomeActive();
    assert(reading.unlocked());

    // Boundary cases exercise separate returns, not a timer that happens to fire.
    for (double elapsed : {0.0, 59.999, 60.0, 60.001, 3600.0}) {
        auto policy = authenticated();
        policy.resignActive();
        policy.enterBackground(100);
        assert(policy.enterForeground(100 + elapsed) == (elapsed >= 60));
        policy.becomeActive();
        assert(policy.unlocked() == (elapsed < 60));
    }

    // A suspended timer need not run: the foreground refresh still locks.
    auto suspended = authenticated();
    suspended.enterBackground(10);
    assert(suspended.refresh(3710));
    assert(!suspended.unlocked());
    assert(!suspended.refresh(3711)); // exactly one newly-locked callback
    suspended.enterForeground(3712);
    suspended.becomeActive();
    assert(!suspended.unlocked());

    // Duplicate background notifications do not extend the original grace.
    auto duplicate = authenticated();
    duplicate.enterBackground(100);
    duplicate.enterBackground(159);
    assert(duplicate.enterForeground(160));

    // Authentication may complete before UIKit restores active state.
    Policy pending;
    pending.becomeActive();
    auto attempt = pending.beginAuthentication();
    assert(pending.beginAuthentication() == 0);
    pending.resignActive();
    assert(pending.finishAuthentication(attempt, true) == Result::pendingActive);
    assert(!pending.unlocked());
    pending.becomeActive();
    assert(pending.unlocked());

    // Backgrounding invalidates successful-but-pending and late replies alike.
    for (bool completesBeforeBackground : {false, true}) {
        Policy policy;
        policy.becomeActive();
        auto stale = policy.beginAuthentication();
        policy.resignActive();
        if (completesBeforeBackground) {
            assert(policy.finishAuthentication(stale, true) == Result::pendingActive);
        }
        policy.enterBackground(100);
        assert(policy.finishAuthentication(stale, true) == Result::ignored);
        policy.enterForeground(101);
        policy.becomeActive();
        assert(!policy.unlocked());
        auto retry = policy.beginAuthentication();
        assert(retry != stale);
        assert(policy.finishAuthentication(stale, true) == Result::ignored);
        assert(policy.finishAuthentication(retry, false) == Result::failed);
        assert(!policy.unlocked());
        retry = policy.beginAuthentication();
        assert(policy.finishAuthentication(retry, true) == Result::unlocked);
    }

    // A cold process cannot inherit an earlier process's grace or access.
    auto oldProcess = authenticated();
    oldProcess.enterBackground(10);
    Policy newProcess;
    newProcess.becomeActive();
    assert(!newProcess.unlocked());
    assert(newProcess.beginAuthentication() != 0);
    std::cout << "App lock policy tests passed\n";
}
