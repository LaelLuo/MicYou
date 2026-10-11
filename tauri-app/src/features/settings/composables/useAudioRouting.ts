import { computed, onMounted, onScopeDispose, ref } from 'vue';
import { command, onEvent, type AudioRoutingStatus, type OutputEndpoint } from '@/platform';
import { useListeners } from '@/shared/lib/listeners';
import { isWindows } from '@/shared/lib/os';

const endpoints = ref<OutputEndpoint[]>([]);
const routingStatus = ref<AudioRoutingStatus | null>(null);
const routingError = ref(false);
const routingBusy = ref(false);
let eventRevision = 0;

export function useAudioRouting() {
  const track = useListeners();
  let disposed = false;
  onScopeDispose(() => { disposed = true; });
  async function refreshRouting() {
    if (disposed || routingBusy.value) return;
    const revision = eventRevision;
    routingBusy.value = true;
    routingError.value = false;
    try {
      const [devices, status] = await Promise.all([
        command('list_output_endpoints'), command('get_audio_routing_status'),
      ]);
      if (disposed) return;
      endpoints.value = devices;
      if (revision === eventRevision) routingStatus.value = status;
    } catch (e) {
      if (!disposed) routingError.value = true;
      console.error('Failed to load audio routing:', e);
    } finally {
      routingBusy.value = false;
    }
  }
  async function selectOutput(endpointId: string) {
    if (disposed || routingBusy.value) return;
    const previous = routingStatus.value;
    const revision = eventRevision;
    routingBusy.value = true;
    routingError.value = false;
    if (previous) routingStatus.value = { ...previous, outputEndpointId: endpointId };
    try {
      const status = await command('set_output_endpoint', { endpointId });
      if (!disposed && revision === eventRevision) routingStatus.value = status;
    } catch (e) {
      if (!disposed && revision === eventRevision) routingStatus.value = previous;
      if (!disposed) routingError.value = true;
      console.error('Failed to change microphone output:', e);
    } finally {
      routingBusy.value = false;
    }
  }
  onMounted(async () => {
    if (!isWindows) return;
    try {
      await track(onEvent('audio-routing-status-changed', (status) => {
        if (disposed) return;
        eventRevision += 1;
        routingStatus.value = status;
      }));
      if (disposed) return;
      await track(onEvent('settings-window-shown', () => { void refreshRouting(); }));
      if (disposed) return;
      await refreshRouting();
    } catch (e) {
      if (!disposed) routingError.value = true;
      console.error('Failed to initialize audio routing listeners:', e);
    }
  });
  return {
    endpoints, routingStatus, routingError, routingBusy, refreshRouting, selectOutput,
    hasCable: computed(() => endpoints.value.some((device) => device.component === 'cable' && device.backingId)),
    hasHifi: computed(() => endpoints.value.some((device) => device.component === 'hifi' && device.backingId)),
  };
}
