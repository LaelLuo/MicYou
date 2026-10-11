import { afterEach, expect, mock, spyOn, test } from 'bun:test';
import * as vue from 'vue';

let monitoringEvent: (enabled: boolean) => void = () => {};
let invoke: () => Promise<void> = () => Promise.resolve();
let commandCount = 0;

mock.module('vue', () => ({ ...vue, onMounted: (callback: () => void) => callback() }));
mock.module('@vueuse/core', () => ({ useStorage: () => vue.ref(false) }));
mock.module('@/shared/lib/listeners', () => ({ useListeners: () => (listener: Promise<() => void>) => listener }));
mock.module('@/platform', () => ({
  command: () => {
    commandCount++;
    return invoke();
  },
  onEvent: (name: string, callback: (enabled: boolean) => void) => {
    if (name === 'monitoring-enabled-changed') monitoringEvent = callback;
    return Promise.resolve(() => {});
  },
}));

const { useAudio } = await import('../src/features/audio/composables/useAudio');
afterEach(() => mock.restore());

test('monitoring UI waits for successful backend event and rejects duplicate clicks', async () => {
  commandCount = 0;
  const pending = Promise.withResolvers<void>();
  invoke = () => pending.promise;
  const audio = useAudio();
  audio.handleMonitoringWarningConfirm(false);
  audio.handleMonitoringWarningConfirm(false);
  expect(commandCount).toBe(1);
  expect(audio.isMonitoringEnabled.value).toBe(false);
  monitoringEvent(true);
  pending.resolve();
  await pending.promise;
  expect(audio.isMonitoringEnabled.value).toBe(true);
});

test('failed request cannot overwrite a successful external monitoring event', async () => {
  spyOn(console, 'error').mockImplementation(() => {});
  const pending = Promise.withResolvers<void>();
  invoke = () => pending.promise;
  const audio = useAudio();
  audio.handleMonitoringWarningConfirm(false);
  monitoringEvent(true);
  pending.reject(new Error('monitoring update already in progress'));
  await pending.promise.catch(() => {});
  expect(audio.isMonitoringEnabled.value).toBe(true);
});
