<script setup lang="ts">
import { useSettingsText } from "../../i18n/settings-general"
defineProps<{ busy: boolean; error: string; saved?: boolean }>()
defineEmits<{ reload: [] }>()
const s = useSettingsText()
</script>

<template>
  <UAlert v-if="busy" class="settings-banner" color="neutral" variant="subtle" :description="s('Working…', '正在处理…')" role="status" />
  <UAlert v-if="error" class="settings-banner" color="error" variant="subtle" :description="error" role="alert">
    <template #actions>
      <UButton color="neutral" variant="outline" size="sm" :disabled="busy" @click="$emit('reload')">{{ s('Reload', '重新加载') }}</UButton>
    </template>
  </UAlert>
  <UAlert v-if="saved && !error" class="settings-banner" color="success" variant="subtle" :description="s('Saved', '已保存')" role="status" />
</template>
