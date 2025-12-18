<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { ICONS } from '../lib/constants';
  import './NavBar.css';
  import '@material/web/icon/icon.js';
  import '@material/web/ripple/ripple.js';

  interface Props {
    activeTab: string;
    onTabChange: (id: string) => void;
  }

  let { activeTab, onTabChange }: Props = $props();
  let navContainer = $state<HTMLElement>();
  let tabRefs = $state<Record<string, HTMLButtonElement>>({});
  const HYMO_ICON = "M2,9c0-1.76,0.59-3.38,1.58-4.7C4.4,2.54,6.58,1.25,9.08,1.02C9.38,1,9.69,1,10,1c4.97,0,9,4.03,9,9v8.03 c0,2.19-1.78,3.97-3.97,3.97h-0.09c-0.84,0-1.63-0.27-2.28-0.73l-0.89-0.63C11.52,20.47,11.27,20.39,11,20.39 c-0.27,0-0.52,0.08-0.74,0.23l-0.95,0.67C8.68,21.75,7.87,22,7.03,22H7c-2.21,0-4-1.79-4-4V9z M9,9c0-1.1-0.9-2-2-2S5,7.9,5,9s0.9,2,2,2 S9,10.1,9,9z M17,9c0-1.1-0.9-2-2-2s-2,0.9-2,2s0.9,2,2,2S17,10.1,17,9z";

  let tabs = $derived([
    { id: 'status', icon: ICONS.home },
    { id: 'config', icon: ICONS.settings },
    { id: 'modules', icon: ICONS.modules },
    ...(store.storage?.hymofs_available ? [{ id: 'hymo', icon: HYMO_ICON }] : []),
    { id: 'logs', icon: ICONS.description },
    { id: 'info', icon: ICONS.info }
  ]);

  $effect(() => {
    if (activeTab && tabRefs[activeTab] && navContainer) {
      const tab = tabRefs[activeTab];
      const containerWidth = navContainer.clientWidth;
      const tabLeft = tab.offsetLeft;
      const tabWidth = tab.clientWidth;
      const scrollLeft = tabLeft - (containerWidth / 2) + (tabWidth / 2);
      
      navContainer.scrollTo({
        left: scrollLeft,
        behavior: 'smooth'
      });
    }
  });
</script>

<nav class="bottom-nav" bind:this={navContainer} style:padding-bottom={store.fixBottomNav ? '48px' : 'max(16px, env(safe-area-inset-bottom, 0px))'}>
  {#each tabs as tab (tab.id)}
    <button 
      class="nav-tab {activeTab === tab.id ? 'active' : ''}" 
      onclick={() => onTabChange(tab.id)}
      bind:this={tabRefs[tab.id]}
      type="button"
    >
      <md-ripple></md-ripple>
      <div class="icon-container">
        <md-icon>
          <svg viewBox="0 0 24 24">
            <path d={tab.icon} style="transition: none" />
          </svg>
        </md-icon>
      </div>
      <span class="label">{store.L.tabs[tab.id as keyof typeof store.L.tabs]}</span>
    </button>
  {/each}
</nav>