import { afterEach, expect, mock, spyOn, test } from 'bun:test';
import * as vue from 'vue';
import type { DownlinkStatus } from '../src/platform/types';

let mounted: () => Promise<void> = async () => {};
let statusEvent: (status: DownlinkStatus) => void = () => {};
let restore: () => Promise<DownlinkStatus> = async () => snapshot('restored');
let calls = 0;
function snapshot(routeState: string): DownlinkStatus {
  return { supported: true, phoneSupported: true, supportsPlaybackRouting: true,
    phoneSupportsPlaybackRouting: true, requested: false, active: false,
    sourceId: 'virtual-output', error: null, droppedFrames: 0,
    routeState, routeError: routeState === 'restore_failed' ? 'route_restore_failed' : null };
}
mock.module('vue', () => ({ ...vue, onMounted: (callback: () => Promise<void>) => { mounted = callback; } }));
mock.module('@/shared/lib/listeners', () => ({ useListeners: () => (listener: Promise<() => void>) => listener }));
mock.module('@/platform', () => ({
  command: async (name: string) => {
    if (name === 'list_downlink_sources') return [];
    if (name === 'retry_playback_route_restore') { calls++; return restore(); }
    return snapshot('restore_failed');
  },
  onEvent: (name: string, callback: (status: DownlinkStatus) => void) => {
    if (name === 'downlink-status-changed') statusEvent = callback;
    return Promise.resolve(() => {});
  },
}));
const { useDownlink } = await import('../src/features/audio/composables/useDownlink');
afterEach(() => mock.restore());

test('restore retries are serialized and an old result cannot overwrite a newer route event', async () => {
  const scope = vue.effectScope();
  const state = scope.run(useDownlink)!;
  await mounted();
  const pending = Promise.withResolvers<DownlinkStatus>();
  restore = () => pending.promise; calls = 0;
  const first = state.retryRestore();
  await state.retryRestore();
  expect(calls).toBe(1);
  statusEvent(snapshot('held'));
  pending.resolve(snapshot('restored'));
  await first;
  expect(state.status.value?.routeState).toBe('held');
  await state.retryRestore();
  expect(calls).toBe(1);
  scope.stop();
});

test('obsolete restore rejection cannot mask a newer successful status', async () => {
  spyOn(console, 'error').mockImplementation(() => {});
  const scope = vue.effectScope();
  const state = scope.run(useDownlink)!;
  await mounted();
  const pending = Promise.withResolvers<DownlinkStatus>();
  restore = () => pending.promise;
  const operation = state.retryRestore();
  statusEvent(snapshot('restored'));
  pending.reject(new Error('late restore result'));
  await operation;
  expect(state.status.value?.routeState).toBe('restored');
  expect(state.operationError.value).toBeNull();
  scope.stop();
});
