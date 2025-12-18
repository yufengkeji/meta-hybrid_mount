<script lang="ts">
  import { onMount } from 'svelte';
  import { API } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import type { HymoStatus } from '../lib/types';
  import './HymoTab.css';
  import '@material/web/switch/switch.js';
  import '@material/web/button/filled-button.js';
  import '@material/web/button/filled-tonal-button.js';
  import '@material/web/icon/icon.js';

  let status = $state<HymoStatus | null>(null);
  let loading = $state(false);
  let activeRuleTab = $state<'redirects' | 'hides' | 'injects' | 'xattrs'>('redirects');

  async function loadStatus() {
    loading = true;
    try {
      status = await API.getHymoStatus();
    } catch (e) {
      console.error(e);
    } finally {
      loading = false;
    }
  }

  async function toggleStealth(e: Event) {
    if (!status) return;
    // Use 'any' to bypass HTMLInputElement type check for custom element properties
    const target = e.target as any;
    const newState = target.selected;
    try {
      await API.setHymoStealth(newState);
      status.stealth_active = newState;
    } catch (error) {
      target.selected = !newState; // revert on error
    }
  }

  async function toggleDebug(e: Event) {
    if (!status) return;
    const target = e.target as any;
    const newState = target.selected;
    try {
      await API.setHymoDebug(newState);
      status.debug_active = newState;
    } catch (error) {
      target.selected = !newState;
    }
  }

  async function reorderMounts() {
    if (!confirm(store.L.hymo.reorderConfirm)) return;
    try {
      await API.triggerMountReorder();
      store.showToast(store.L.hymo.reorderSuccess, 'success');
    } catch (e) {
      store.showToast(store.L.hymo.reorderFail, 'error');
    }
  }

  onMount(() => {
    loadStatus();
  });
</script>

<div class="hymo-container">
  <div class="status-card">
    <div class="status-header">
      <div class="status-icon" class:active={status?.available}>
        <md-icon>
          {#if status?.available}check_circle{:else}cancel{/if}
        </md-icon>
      </div>
      <div class="status-info">
        <h2>{store.L.hymo.title}</h2>
        <p>
          {#if status?.available}
            Protocol v{status.protocol_version} • Config v{status.config_version}
          {:else}
            {store.L.hymo.notAvailable}
          {/if}
        </p>
      </div>
    </div>

    {#if status?.available}
      <div class="control-group">
        <div class="control-item">
          <div class="control-label">
            <span class="control-title">{store.L.config.hymoStealth}</span>
            <span class="control-desc">{store.L.hymo.stealthDesc}</span>
          </div>
          <md-switch selected={status.stealth_active} onchange={toggleStealth}></md-switch>
        </div>
        <div class="control-item">
          <div class="control-label">
            <span class="control-title">{store.L.config.hymoDebug}</span>
            <span class="control-desc">{store.L.hymo.debugDesc}</span>
          </div>
          <md-switch selected={status.debug_active} onchange={toggleDebug}></md-switch>
        </div>
      </div>

      <div class="action-row">
        <md-filled-tonal-button onclick={reorderMounts} style="flex: 1">
          <md-icon slot="icon">shuffle</md-icon>
          {store.L.hymo.reorderBtn}
        </md-filled-tonal-button>
        <md-filled-button onclick={loadStatus}>
          <md-icon slot="icon">refresh</md-icon>
          {store.L.modules.reload}
        </md-filled-button>
      </div>
    {/if}
  </div>

  {#if status?.available}
    <div class="rules-card">
      <div class="tabs-header">
        <button 
          class="tab-btn" 
          class:active={activeRuleTab === 'redirects'}
          onclick={() => activeRuleTab = 'redirects'}>
          Redirects ({status.rules.redirects.length})
        </button>
        <button 
          class="tab-btn" 
          class:active={activeRuleTab === 'hides'}
          onclick={() => activeRuleTab = 'hides'}>
          Hides ({status.rules.hides.length})
        </button>
        <button 
          class="tab-btn" 
          class:active={activeRuleTab === 'injects'}
          onclick={() => activeRuleTab = 'injects'}>
          Injects ({status.rules.injects.length})
        </button>
        <button 
          class="tab-btn" 
          class:active={activeRuleTab === 'xattrs'}
          onclick={() => activeRuleTab = 'xattrs'}>
          Xattrs ({status.rules.xattr_sbs.length})
        </button>
      </div>

      <div class="rules-list">
        {#if activeRuleTab === 'redirects'}
          {#each status.rules.redirects as rule}
            <div class="rule-item">
              <div class="rule-primary">{rule.src}</div>
              <div class="rule-secondary">
                <span class="arrow">↳</span> {rule.target}
              </div>
            </div>
          {:else}
            <div class="empty-state">{store.L.modules.empty}</div>
          {/each}
        
        {:else if activeRuleTab === 'hides'}
          {#each status.rules.hides as path}
            <div class="rule-item">
              <div class="rule-primary">{path}</div>
              <div class="rule-secondary">Hidden</div>
            </div>
          {:else}
            <div class="empty-state">{store.L.modules.empty}</div>
          {/each}

        {:else if activeRuleTab === 'injects'}
          {#each status.rules.injects as path}
            <div class="rule-item">
              <div class="rule-primary">{path}</div>
              <div class="rule-secondary">Injection Parent</div>
            </div>
          {:else}
            <div class="empty-state">{store.L.modules.empty}</div>
          {/each}

        {:else if activeRuleTab === 'xattrs'}
          {#each status.rules.xattr_sbs as sb}
            <div class="rule-item">
              <div class="rule-primary">SuperBlock: {sb}</div>
              <div class="rule-secondary">Overlay Xattrs Hidden</div>
            </div>
          {:else}
            <div class="empty-state">{store.L.modules.empty}</div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</div>