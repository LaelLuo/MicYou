<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { AcceptableValue } from 'reka-ui';
import { RefreshCw } from '@lucide/vue';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select';
import { useDownlink } from '../composables/useDownlink';

const { t } = useI18n();
const { sources, status, loading, saving, operationError, refresh, selectSource, retryRestore } = useDownlink();
const defaultValue = '__default_downlink_source__';
const selectedSource = computed(() => status.value?.sourceId ?? defaultValue);
const selectedMissing = computed(() => status.value?.sourceId && !sources.value.some((source) => source.id === status.value?.sourceId));
const busy = computed(() => loading.value || saving.value);
const errorKeys = new Set([
  'unsupported_platform', 'unsupported_peer', 'source_unavailable', 'feedback_source',
  'capture_failed', 'invalid_frame', 'transport_stalled', 'load_failed', 'save_failed',
  'identity_unavailable',
  'route_restore_failed',
  'route_source_required', 'route_restore_required', 'route_acquire_failed',
  'route_changed_external', 'route_busy', 'route_restore_target_required', 'route_unavailable',
  'playback_failed', 'builtin_microphone_required', 'unsupported_android',
]);
const errorMessage = computed(() => {
  const error = operationError.value ?? (status.value?.routeState === 'restore_failed'
    ? 'route_restore_failed' : status.value?.routeError ?? status.value?.error);
  const code = error?.split(':', 1)[0];
  if (code === 'identity_unavailable') return t('routing.identityError');
  return code ? t(`downlink.errors.${errorKeys.has(code) ? code : 'unknown'}`) : null;
});
const stateLabel = computed(() => {
  if (busy.value) return t('downlink.states.loading');
  if (!status.value) return t('downlink.states.unavailable');
  if (!status.value.supported) return t('downlink.states.unsupported');
  if (status.value.error || status.value.routeState === 'restore_failed') return t('downlink.states.error');
  if (status.value.active) return t('downlink.states.active');
  if (status.value.requested) return t('downlink.states.starting');
  return t('downlink.states.idle');
});
const routeKeys = new Set(['idle', 'acquiring', 'held', 'restored', 'restore_failed', 'released_external']);
const routeLabel = computed(() => {
  if (!status.value?.supportsPlaybackRouting) return null;
  if (status.value.routeState && routeKeys.has(status.value.routeState) && status.value.routeState !== 'idle') {
    return t(`downlink.routes.${status.value.routeState}`);
  }
  return t(status.value.phoneSupported && !status.value.phoneSupportsPlaybackRouting
    ? 'downlink.routes.unsupported_peer' : 'downlink.routes.idle');
});

function updateSource(value: AcceptableValue) {
  if (typeof value !== 'string') return;
  void selectSource(value === defaultValue ? null : value);
}
</script>

<template>
  <section class="bg-surface-bright rounded-2xl p-4 shadow-xs space-y-3" aria-labelledby="downlink-title">
    <div class="flex items-start justify-between gap-3">
      <div>
        <h4 id="downlink-title" class="font-bold text-on-surface">{{ t('downlink.title') }}</h4>
        <p class="text-xs text-on-surface-variant mt-1">{{ t('downlink.description') }}</p>
      </div>
      <button
        type="button"
        class="rounded-full p-2 text-primary hover:bg-surface-container disabled:opacity-40 shrink-0"
        :disabled="busy"
        :aria-label="t('downlink.refresh')"
        :title="t('downlink.refresh')"
        @click="refresh"
      >
        <RefreshCw class="w-4 h-4" :class="{ 'animate-spin': loading }" />
      </button>
    </div>
    <label id="downlink-source-label" class="block text-sm font-medium text-on-surface">{{ t('downlink.source') }}</label>
    <Select :model-value="selectedSource" :disabled="busy || !status?.supported" @update:model-value="updateSource">
      <SelectTrigger aria-labelledby="downlink-source-label" class="w-full bg-surface-container border-none shadow-none rounded-xl h-12 px-4 font-medium text-sm">
        <SelectValue />
      </SelectTrigger>
      <SelectContent class="border-surface-variant/20 rounded-xl bg-surface shadow-lg max-h-[40vh]">
        <SelectItem :value="defaultValue" :disabled="status?.supportsPlaybackRouting && status?.phoneSupportsPlaybackRouting">{{ t('downlink.defaultSource') }}</SelectItem>
        <SelectItem v-if="selectedMissing && status?.sourceId" :value="status.sourceId" disabled>{{ t('downlink.missingSource') }}</SelectItem>
        <SelectItem v-for="source in sources" :key="source.id" :value="source.id" :disabled="source.isFeedback">
          {{ source.name }}{{ source.isDefault ? ` · ${t('downlink.defaultBadge')}` : '' }}{{ source.isFeedback ? ` · ${t('downlink.feedbackBadge')}` : '' }}
        </SelectItem>
      </SelectContent>
    </Select>
    <p class="text-xs text-on-surface-variant" role="status" aria-live="polite">{{ stateLabel }}</p>
    <p v-if="routeLabel" class="text-xs" :class="status?.routeState === 'restore_failed' ? 'text-error' : 'text-on-surface-variant'" role="status" aria-live="polite">{{ routeLabel }}</p>
    <button v-if="status?.routeState === 'restore_failed'" type="button" :disabled="busy"
      class="rounded-full bg-primary text-on-primary px-4 py-2 text-sm disabled:opacity-40" @click="retryRestore">
      {{ t('downlink.retryRestore') }}
    </button>
    <p v-if="errorMessage" class="text-xs text-error" role="alert">{{ errorMessage }}</p>
    <details v-if="status?.routeError" class="text-xs text-on-surface-variant">
      <summary class="cursor-pointer">{{ t('downlink.errorDetails') }}</summary>
      <p class="mt-2 break-all select-text">{{ status.routeError }}</p>
    </details>
  </section>
</template>
