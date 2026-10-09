<template>
  <Teleport to="body">
    <div v-if="visible" class="disc-mask">
      <div class="disc-panel">
        <div class="disc-head">
          <div class="disc-icon">
            <svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M12 9v4" /><path d="M12 17h.01" />
              <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
            </svg>
          </div>
          <div>
            <h3 class="disc-title">使用须知</h3>
            <p class="disc-sub">只读展示工具 · 请花 10 秒了解边界</p>
          </div>
        </div>

        <div class="disc-body">
          <p class="disc-lead">
            本工具是一个<strong>本地只读的展示工具</strong>：用你自己的登录会话，把你账号的额度与用量
            <strong>元数据</strong>（token 数 / 费用 / 状态码 / 模型名 / 时间）同步到本地，再用卡片和图表展示出来。
          </p>
          <ul class="disc-list">
            <li><strong>只读</strong>：不修改云端数据、不发起推理、不做代理转发（不是 AI 网关）。</li>
            <li><strong>不绕过</strong>：不做额度 / 限流 / 计费 / 风控的绕过，<strong>不自动切换或轮换账号</strong>，不代他人操作。</li>
            <li><strong>不碰内容、不上传</strong>：不读取也不存储提示词与模型输出；没有遥测与云端中转，数据只在你本机。</li>
          </ul>
          <p class="disc-warn">
            一点提醒：通过第三方客户端访问控制台接口，可能触及 OpenCode
            <a href="https://opencode.ai/zh/legal/terms-of-service" target="_blank" rel="noreferrer">服务条款</a>
            里「程序化提取数据 / 抓取 / 逆向工程 / 多账号」相关表述。是否使用请你自行判断，作者不提供担保；
            建议只添加你本人拥有或已获授权访问的账号。
          </p>
        </div>

        <div class="disc-actions">
          <BaseButton @click="$emit('decline')">退出</BaseButton>
          <BaseButton variant="primary" @click="$emit('accept')">我已知晓</BaseButton>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<script setup>
import BaseButton from './base/BaseButton.vue'

defineProps({
  visible: { type: Boolean, default: false },
})
defineEmits(['accept', 'decline'])
</script>

<style scoped>
.disc-mask {
  position: fixed; inset: 0; z-index: 200;
  background: rgba(6, 10, 18, .72);
  display: flex; align-items: center; justify-content: center;
  padding: 24px;
}
.disc-panel {
  width: 640px; max-width: 100%; max-height: 88vh; overflow: auto;
  background: var(--panel); border: 1px solid var(--border); border-radius: 12px;
  box-shadow: 0 24px 60px rgba(0, 0, 0, .45);
  padding: 20px 22px 18px;
}
.disc-head { display: flex; align-items: center; gap: 12px; margin-bottom: 12px; }
.disc-icon { color: #e0a83c; display: flex; }
.disc-title { font-size: 15px; font-weight: 600; color: var(--text); margin: 0; }
.disc-sub { font-size: 11px; color: var(--muted); margin: 2px 0 0; }
.disc-body { font-size: 12px; line-height: 1.75; color: var(--muted); }
.disc-body strong { color: var(--text); font-weight: 600; }
.disc-body a { color: var(--blue); }
.disc-lead { margin: 0 0 8px; }
.disc-list { margin: 0 0 10px; padding-left: 18px; }
.disc-list li { margin: 2px 0; }
.disc-warn {
  margin: 0 0 8px; padding: 10px 12px;
  background: rgba(224, 168, 60, .10);
  border: 1px solid rgba(224, 168, 60, .35);
  border-radius: 8px; color: var(--muted);
}
.disc-ask { margin: 0; }
.disc-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 16px; }
</style>
