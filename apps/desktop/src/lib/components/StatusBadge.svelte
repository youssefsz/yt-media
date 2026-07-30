<script lang="ts">
  import type { JobDto } from '../ipc/generated';
  import { stateLabel } from '../presentation';

  export let state: JobDto['state'];
</script>

<span class="status-badge" data-state={state}>
  <span class="status-dot" aria-hidden="true"></span>
  {stateLabel(state)}
</span>
