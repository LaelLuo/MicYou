import { onMounted, onScopeDispose, ref } from 'vue';
import { command, onEvent, type DownlinkSource, type DownlinkStatus } from '@/platform';
import { useListeners } from '@/shared/lib/listeners';

export function useDownlink() {
  const sources = ref<DownlinkSource[]>([]);
  const status = ref<DownlinkStatus | null>(null);
  const loading = ref(false);
  const saving = ref(false);
  const operationError = ref<string | null>(null);
  const track = useListeners();
  let disposed = false;
  let eventRevision = 0;

  onScopeDispose(() => { disposed = true; });

  async function refresh() {
    if (loading.value || saving.value || disposed) return;
    loading.value = true;
    operationError.value = null;
    const revision = eventRevision;
    try {
      const [devices, snapshot] = await Promise.all([
        command('list_downlink_sources'),
        command('get_downlink_status'),
      ]);
      if (disposed) return;
      sources.value = devices;
      // 事件在请求途中到达时，快照可能属于较早状态。
      if (revision === eventRevision) status.value = snapshot;
    } catch (error) {
      if (!disposed) operationError.value = 'load_failed';
      console.error('Failed to refresh computer audio:', error);
    } finally {
      if (!disposed) loading.value = false;
    }
  }

  async function selectSource(sourceId: string | null) {
    if (loading.value || saving.value || disposed || sourceId === status.value?.sourceId) return;
    saving.value = true;
    operationError.value = null;
    const revision = eventRevision;
    try {
      const snapshot = await command('set_downlink_source', { sourceId });
      if (!disposed && revision === eventRevision) status.value = snapshot;
    } catch (error) {
      if (!disposed) operationError.value = 'save_failed';
      console.error('Failed to select computer audio source:', error);
    } finally {
      if (!disposed) saving.value = false;
    }
  }

  async function retryRestore() {
    if (loading.value || saving.value || disposed || status.value?.routeState !== 'restore_failed') return;
    saving.value = true;
    operationError.value = null;
    const revision = eventRevision;
    try {
      const snapshot = await command('retry_playback_route_restore');
      if (!disposed && revision === eventRevision) status.value = snapshot;
    } catch (error) {
      if (!disposed && revision === eventRevision) operationError.value = 'route_restore_failed';
      console.error('Failed to restore computer output:', error);
    } finally {
      if (!disposed) saving.value = false;
    }
  }

  onMounted(async () => {
    let listenerFailed = false;
    try {
      await Promise.all([
        track(onEvent('downlink-status-changed', (snapshot) => {
          if (disposed) return;
          eventRevision += 1;
          status.value = snapshot;
        })),
        track(onEvent('settings-window-shown', () => { void refresh(); })),
      ]);
    } catch (error) {
      listenerFailed = true;
      console.error('Failed to listen for computer audio status:', error);
    }
    await refresh();
    if (!disposed && listenerFailed) operationError.value = 'load_failed';
  });

  return { sources, status, loading, saving, operationError, refresh, selectSource, retryRestore };
}
