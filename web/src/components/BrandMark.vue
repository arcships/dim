<script setup lang="ts">
// DimCode brand mark, the six ring cells of Desktop's BrandMark. Colored via currentColor.
// `animated`: an orbit highlight travels clockwise around the ring, the startup loading
// indicator. index.html draws the same mark before the bundle loads; keep them in step.
withDefaults(defineProps<{ animated?: boolean }>(), { animated: false })

const cells = [
  { delay: "-1.6s", d: "M52.042 34C50.8561 34.0001 49.8945 34.9615 49.8945 36.1475V46.1689C49.8946 47.3549 50.8561 48.3163 52.042 48.3164H77.958C79.144 48.3164 80.1054 47.3549 80.1055 46.1689V36.1475C80.1055 34.9615 79.144 34 77.958 34H52.042Z" },
  { delay: "-1.333s", d: "M83.8311 49.8945C82.6452 49.8947 81.6836 50.8561 81.6836 52.042V62.0635C81.6838 63.2493 82.6453 64.2108 83.8311 64.2109H93.8525C95.0384 64.2109 95.9998 63.2493 96 62.0635V52.042C96 50.856 95.0385 49.8945 93.8525 49.8945H83.8311Z" },
  { delay: "-1.067s", d: "M83.8311 65.7891C82.6453 65.7892 81.6838 66.7508 81.6836 67.9365V77.958C81.6836 79.1439 82.6452 80.1053 83.8311 80.1055H93.8525C95.0385 80.1055 96 79.144 96 77.958V67.9365C95.9998 66.7507 95.0384 65.7891 93.8525 65.7891H83.8311Z" },
  { delay: "-0.8s", d: "M52.042 81.6836C50.8561 81.6837 49.8946 82.6452 49.8945 83.8311V93.8525C49.8945 95.0385 50.8561 95.9999 52.042 96H77.958C79.144 96 80.1055 95.0385 80.1055 93.8525V83.8311C80.1054 82.6451 79.1439 81.6836 77.958 81.6836H52.042Z" },
  { delay: "-0.533s", d: "M36.1475 65.7891C34.9616 65.7891 34.0002 66.7507 34 67.9365V77.958C34 79.144 34.9615 80.1055 36.1475 80.1055H46.1689C47.3548 80.1054 48.3164 79.1439 48.3164 77.958V67.9365C48.3162 66.7507 47.3547 65.7892 46.1689 65.7891H36.1475Z" },
  { delay: "-0.267s", d: "M36.1475 49.8945C34.9615 49.8945 34 50.856 34 52.042V62.0635C34.0002 63.2493 34.9616 64.2109 36.1475 64.2109H46.1689C47.3547 64.2108 48.3162 63.2493 48.3164 62.0635V52.042C48.3164 50.8561 47.3548 49.8946 46.1689 49.8945H36.1475Z" },
]
</script>

<template>
  <svg viewBox="34 34 62 62" aria-hidden="true" class="brand-mark" :class="{ 'is-animated': animated }">
    <path v-for="cell in cells" :key="cell.d" fill="currentColor" :d="cell.d" :style="{ animationDelay: cell.delay }" />
  </svg>
</template>

<style scoped>
.brand-mark.is-animated path { opacity: .26; animation: brand-mark-orbit 1.6s linear infinite; }
@keyframes brand-mark-orbit {
  0% { opacity: .26; }
  12% { opacity: 1; }
  46% { opacity: .26; }
  100% { opacity: .26; }
}
@media (prefers-reduced-motion: reduce) {
  .brand-mark.is-animated path { opacity: 1; animation: none; }
}
</style>
