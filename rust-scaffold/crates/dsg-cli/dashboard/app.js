"use strict";

const refresh = document.getElementById("refresh");
const overview = document.getElementById("overview");
const feedback = document.getElementById("feedback");
const observedAt = document.getElementById("observed-at");
const usageMeter = document.getElementById("usage-meter");
const scanNote = document.getElementById("scan-note");
const numberFormat = new Intl.NumberFormat("ko-KR", { maximumFractionDigits: 1 });
const dateFormat = new Intl.DateTimeFormat("ko-KR", { dateStyle: "medium", timeStyle: "short" });
const byteUnits = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
const requestTimeoutMs = 30_000;
const budgetStates = new Map([
  ["normal", "정상"],
  ["warning", "주의"],
  ["critical", "위험"],
]);
let loading = false;

function isQuantity(value) {
  return Number.isInteger(value) && value >= 0;
}

function withUnit(value, unit) {
  return isQuantity(value) ? `${numberFormat.format(value)}${unit}` : null;
}

function formatBytes(bytes) {
  if (!isQuantity(bytes)) return null;

  let scaled = bytes;
  let unit = 0;
  while (scaled >= 1024 && unit < byteUnits.length - 1) {
    scaled /= 1024;
    unit += 1;
  }
  return `${numberFormat.format(scaled)} ${byteUnits[unit]}`;
}

function setMetric(id, label) {
  const target = document.getElementById(id);
  target.textContent = label ?? "미확인";
  target.toggleAttribute("data-unknown", label === null);
}

function setStatus(id, label, tone = "unknown") {
  const target = document.getElementById(id);
  target.textContent = label;
  target.dataset.tone = tone;
}

function setObservation(seconds) {
  const date = isQuantity(seconds) ? new Date(seconds * 1000) : null;
  if (!date || Number.isNaN(date.getTime())) {
    observedAt.textContent = "미확인";
    return;
  }
  const time = document.createElement("time");
  time.textContent = dateFormat.format(date);
  time.dateTime = date.toISOString();
  observedAt.replaceChildren(time);
}

function render({ health, policy, docker, cleanup }) {
  const percent = health?.complete === true ? health.project_usage_percent : null;
  const budgetStatus = health?.budget_status;
  const budgetLabel = budgetStates.get(budgetStatus);
  setStatus("budget-status", budgetLabel ?? "미확인", budgetLabel ? budgetStatus : "unknown");
  setMetric("project-size", health?.complete === true ? formatBytes(health.total_bytes) : null);
  setMetric("disk-free", formatBytes(health?.disk_free_bytes));
  setMetric("usage-percent", withUnit(percent, "%"));
  setObservation(health?.observed_at_unix);
  usageMeter.hidden = !isQuantity(percent);
  if (!usageMeter.hidden) {
    usageMeter.value = Math.min(percent, 100);
    usageMeter.setAttribute("aria-valuetext", withUnit(percent, "%"));
  }
  scanNote.hidden = health?.complete !== false;

  const inventory = docker?.complete === true && docker.status === "complete" ? docker : null;
  setStatus("docker-status", inventory ? "확인 완료" : "미확인", inventory ? "normal" : "unknown");
  setMetric("docker-containers", withUnit(inventory?.container_count, "개"));
  setMetric("docker-images", withUnit(inventory?.image_count, "개"));
  setMetric("docker-volumes", withUnit(inventory?.volume_count, "개"));
  setMetric("docker-duplicates", withUnit(inventory?.duplicate_groups, "개"));

  const plan = cleanup?.dry_run === true ? cleanup : null;
  setMetric("cleanup-candidates", withUnit(plan?.candidate_count, "건"));
  setMetric("cleanup-size", formatBytes(plan?.candidate_bytes));
  setMetric("cleanup-excluded", withUnit(plan?.excluded_count, "건"));

  setMetric("policy-budget", withUnit(policy?.project_budget_gb, " GiB"));
  setMetric("policy-warning", withUnit(policy?.warning_percent, "%"));
  setMetric("policy-critical", withUnit(policy?.critical_percent, "%"));
}

function announce(message, tone = "") {
  feedback.textContent = message;
  feedback.dataset.tone = tone;
}

async function loadOverview() {
  if (loading) return;

  loading = true;
  refresh.setAttribute("aria-disabled", "true");
  overview.setAttribute("aria-busy", "true");
  announce("상태를 조회하는 중입니다.");

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), requestTimeoutMs);
  try {
    const response = await fetch("/api/overview", {
      cache: "no-store",
      mode: "same-origin",
      redirect: "error",
      signal: controller.signal,
    });
    if (!response.ok) throw new Error("overview unavailable");

    const data = await response.json();
    if (!data || typeof data !== "object" || Array.isArray(data)) throw new Error("invalid overview");
    render(data);
    announce("상태를 갱신했습니다.");
  } catch {
    render({});
    announce("상태를 불러오지 못했습니다. 새로고침으로 다시 시도해 주세요.", "error");
  } finally {
    clearTimeout(timeout);
    loading = false;
    refresh.removeAttribute("aria-disabled");
    overview.setAttribute("aria-busy", "false");
  }
}

refresh.addEventListener("click", loadOverview);
loadOverview();
