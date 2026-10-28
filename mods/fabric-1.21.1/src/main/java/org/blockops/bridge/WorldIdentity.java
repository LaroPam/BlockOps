package org.blockops.bridge;

import java.lang.ref.WeakReference;

/** Pins the first observed world object without retaining it after the game closes. */
public final class WorldIdentity<T> {
    private WeakReference<T> first;

    public synchronized boolean accepts(T candidate) {
        if (candidate == null) return false;
        if (first == null) {
            first = new WeakReference<>(candidate);
            return true;
        }
        return first.get() == candidate;
    }
}
