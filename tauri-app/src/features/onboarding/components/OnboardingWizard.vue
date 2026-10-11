<script setup lang="ts">
import { ref } from 'vue';
import { X } from '@lucide/vue';
import { isWindows } from '@/shared/lib/os';
import VirtualDeviceCard from '@/features/settings/components/VirtualDeviceCard.vue';
defineProps<{ visible: boolean }>();
const emit = defineEmits<{ complete: [] }>();
const step = ref(1);
function complete() {
  localStorage.setItem('micyou_onboarding_completed', 'true');
  emit('complete');
}
</script>

<template>
  <div v-if="visible" class="fixed inset-0 z-100 flex items-center justify-center bg-black/60 backdrop-blur-xs">
    <div class="bg-surface rounded-3xl shadow-2xl w-full max-w-lg mx-4 overflow-hidden">
      <div class="flex items-center justify-between p-5 border-b border-outline-variant/20">
        <span class="text-sm text-on-surface-variant">{{ step }} / 2</span>
        <button @click="complete" :aria-label="$t('dialogs.close')" class="text-on-surface-variant hover:text-on-surface"><X class="w-5 h-5" /></button>
      </div>
      <div class="p-6 space-y-5 max-h-[60vh] overflow-y-auto">
        <template v-if="step === 1">
          <h2 class="text-xl font-bold text-on-surface">{{ $t('onboarding.title') }}</h2>
          <p class="text-sm text-on-surface-variant">{{ $t('onboarding.desc') }}</p>
          <div v-for="n in [1, 2, 3, 4]" :key="n" class="bg-surface-bright rounded-xl p-3">
            <p class="text-sm font-bold text-on-surface">{{ $t(`onboarding.step${n}Title`) }}</p>
            <template v-if="n === 2">
              <p class="text-xs text-on-surface-variant">{{ $t('onboarding.step2WifiDesc') }}</p>
              <p class="text-xs text-on-surface-variant">{{ $t('onboarding.step2UsbDesc') }}</p>
            </template>
            <p v-else class="text-xs text-on-surface-variant">{{ $t(`onboarding.step${n}Desc`) }}</p>
          </div>
        </template>
        <template v-else-if="isWindows">
          <h2 class="text-xl font-bold text-on-surface">{{ $t('routing.components') }}</h2>
          <VirtualDeviceCard />
          <p class="text-sm text-on-surface-variant">{{ $t('routing.setupHint') }}</p>
        </template>
        <template v-else>
          <h2 class="text-xl font-bold text-on-surface">{{ $t('onboarding.complete') }}</h2>
          <p class="text-sm text-on-surface-variant">{{ $t('onboarding.completeDesc') }}</p>
        </template>
      </div>
      <div class="p-5 border-t border-outline-variant/20 flex justify-between">
        <button @click="complete" class="px-4 py-2 text-sm text-on-surface-variant">{{ $t('onboarding.gotIt') }}</button>
        <button @click="step === 1 ? step = 2 : complete()" class="px-6 py-2 bg-primary rounded-xl text-sm font-bold text-on-primary">{{ $t(step === 1 ? 'onboarding.next' : 'onboarding.startUsing') }}</button>
      </div>
    </div>
  </div>
</template>
