/* Testes de states.js fora do GNOME Shell: `gjs tests/run.js` */
'use strict';

const { Gio } = imports.gi;

const here = Gio.File.new_for_path(imports.system.programPath ?? '.').get_parent();
imports.searchPath.unshift(here.get_parent().get_child('hearphant@gab-simon.github.io').get_path());
const States = imports.states;

let failures = 0;
let count = 0;

function test(name, fn) {
    count++;
    try {
        fn();
        print(`ok   ${name}`);
    } catch (e) {
        failures++;
        print(`FAIL ${name}\n     ${e.message}`);
    }
}

function eq(actual, expected, what = '') {
    if (actual !== expected)
        throw new Error(`${what} esperado ${JSON.stringify(expected)}, obtido ${JSON.stringify(actual)}`);
}

function view(raw) {
    const status = States.normalizeStatus(raw);
    return [status, States.describe(status)];
}

test('serviço ausente → UNAVAILABLE, sem dose', () => {
    const [s, v] = view(null);
    eq(s.mascotState, 'UNAVAILABLE');
    eq(s.dosePercent, null);
    eq(v.ringFraction, null);
    eq(v.headline, 'Serviço indisponível');
});

test('RF-008: dose enviada com fonte UNVERIFIED é descartada', () => {
    const [s, v] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'NORMAL', dose_percent: 43 });
    eq(s.dosePercent, null);
    eq(s.mascotState, 'UNVERIFIED', 'NORMAL (verde) não pode aparecer sem dose:');
    eq(v.headline, 'Dose indisponível: calibre seu dispositivo');
    eq(v.ringFraction, null);
    eq(v.showQuestionBadge, true);
});

test('estados de dose sem dose e sem medição viram ERROR', () => {
    const [s, v] = view({ measurement_status: 'UNAVAILABLE', mascot_state: 'WARNING' });
    eq(s.mascotState, 'ERROR');
    eq(v.headline, 'Monitoramento indisponível', 'ERROR não deve pedir calibração:');
});

test('dose calibrada é exibida com fonte e janela', () => {
    const [s, v] = view({
        measurement_status: 'CALIBRATED', mascot_state: 'NORMAL', dose_percent: 43.2,
        window_hours: 168, coverage_fraction: 1,
    });
    eq(s.mascotState, 'NORMAL');
    eq(v.headline, '43% da referência');
    eq(v.sourceLine, 'Últimos 7 dias · Fonte calibrada');
    eq(Math.abs(v.ringFraction - 0.432) < 1e-9, true, 'ringFraction ≈ 0.432:');
});

test('excedente não é truncado no texto (6.5)', () => {
    const [, v] = view({ measurement_status: 'CALIBRATED', mascot_state: 'EXCEEDED', dose_percent: 125 });
    eq(v.headline, '125% da referência');
    eq(v.ringFraction, 1.25);
});

test('estimativa mostra faixa e incerteza; calibrado não', () => {
    const est = { measurement_status: 'ESTIMATED', mascot_state: 'NOTICE', dose_percent: 55, dose_low_percent: 17.4, dose_high_percent: 173.9, dose_uncertainty_db: 5 };
    const [s, v] = view(est);
    eq(v.sourceLine, 'Últimos 7 dias · Estimativa automática (±5 dB)');
    eq(v.hero, '≈ 55%');
    eq(v.heroCaption, 'da referência semanal · estimativa, faixa 17% a 174%');
    eq(v.doseChip, null, 'a faixa vai na legenda, sem chip separado:');
    eq(v.rangeText, 'faixa 17% a 174%');
    eq(v.estimated, true);
    eq(v.calibrated, false);
    eq(s.doseLow, 17.4);
    const [c] = view({ ...est, measurement_status: 'CALIBRATED' });
    eq(c.doseLow, null);
    eq(c.doseUncertaintyDb, null);
});

test('estimativa sem faixa não vira número', () => {
    const [s, v] = view({ measurement_status: 'ESTIMATED', mascot_state: 'WARNING', dose_percent: 90 });
    eq(s.dosePercent, null);
    eq(s.mascotState, 'UNVERIFIED', 'WARNING sem dose:');
    eq(v.hero.includes('%'), false, 'herói sem %:');
});

test('dose estimada menor que 1% não aparece como 0%', () => {
    const [, v] = view({ measurement_status: 'ESTIMATED', mascot_state: 'NORMAL', dose_percent: 0.3, dose_low_percent: 0.1, dose_high_percent: 0.9, dose_uncertainty_db: 5 });
    eq(v.hero, '≈ < 1%');
    eq(v.heroCaption, 'da referência semanal · estimativa, faixa abaixo de 1%');
});

test('chip pede a análise do sinal quando há perfil', () => {
    const [, v] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'IDLE', estimate_available: true, estimate_profile: 'X', estimate_uncertainty_db: 5, signal_analysis: false });
    eq(v.doseChipAction, true);
    eq(v.doseChip.startsWith('Ative a análise do sinal'), true, 'chip:');
    eq(v.deviceTileCaption, 'Estimativa ±5 dB');
});

test('caixa de som explica a ausência de estimativa', () => {
    const reason = 'Caixas de som dependem da distância e da sala; não há estimativa sem medição no local.';
    const [, v] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'IDLE', estimate_available: false, estimate_reason: reason, device_kind: 'speaker', device_name: 'Caixa' });
    eq(v.doseChip, reason);
    eq(v.doseChipAction, false);
    eq(v.deviceTileCaption, 'Sem estimativa (caixa)');
    eq(v.calibrationLine, reason);
});

test('tipo presumido aparece na linha de calibração', () => {
    const [, v] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'IDLE', estimate_available: true, estimate_profile: 'Fone genérico (EN 50332)', estimate_uncertainty_db: 10, kind_presumed: true, signal_analysis: true });
    eq(v.calibrationLine.startsWith('Estimativa automática: Fone genérico (EN 50332) (±10 dB). Tipo presumido'), true, 'linha:');
});

test('cobertura parcial é declarada', () => {
    const [, v] = view({ measurement_status: 'CALIBRATED', mascot_state: 'NORMAL', dose_percent: 10, coverage_fraction: 0.62 });
    eq(v.coverageLine.startsWith('Cobertura: 62% da semana.'), true, 'coverageLine');
});

test('volume é rotulado como digital, nunca dBA', () => {
    const [, v] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'UNVERIFIED', volume_percent: 22, muted: true });
    eq(v.volumeLine, 'Volume do sistema: 22% · mudo (valor digital, não é dBA)');
});

test('estado desconhecido vira ERROR', () => {
    const [s] = view({ measurement_status: 'CALIBRATED', mascot_state: 'HAPPY', dose_percent: 10 });
    eq(s.mascotState, 'ERROR');
});

test('serviço não pode se passar por UNAVAILABLE local', () => {
    const [s] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'UNAVAILABLE' });
    eq(s.mascotState, 'ERROR');
});

test('demonstração aparece no nome acessível', () => {
    const [, v] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'UNVERIFIED', is_demo: true });
    eq(v.accessibleName.endsWith('(demonstração, dados fictícios)'), true, 'accessibleName');
});

test('tipos inválidos são ignorados', () => {
    const [s] = view({ measurement_status: 'CALIBRATED', mascot_state: 'NORMAL', dose_percent: 'muito', volume_percent: NaN, protection_mode: 'turbo' });
    eq(s.dosePercent, null);
    eq(s.volumePercent, null);
    eq(s.protectionMode, null);
    eq(s.mascotState, 'UNVERIFIED');
});


test('sem dose, o herói é o tempo de escuta, nunca uma porcentagem', () => {
    const [, v] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'UNVERIFIED', listening_ms: 22800000, signal_listening_ms: 22800000, playing: true });
    eq(v.hero, '6 h 20 min');
    eq(v.heroCaption, 'de áudio nos últimos 7 dias');
    eq(v.hero.includes('%'), false, 'herói sem %:');
    eq(v.doseChip, 'Dose indisponível para esta saída');
    eq(v.mood, 'Ouvindo agora');
});

test('tempo aproximado por stream é rotulado', () => {
    const [, v] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'IDLE', listening_ms: 600000, signal_listening_ms: 0 });
    eq(v.heroCaption, 'de áudio em 7 dias (aproximado)');
    eq(v.mood, 'Em silêncio');
});

test('com dose calibrada, o herói é a dose e não há chip', () => {
    const [, v] = view({ measurement_status: 'CALIBRATED', mascot_state: 'NORMAL', dose_percent: 43, listening_ms: 1000, device_name: 'X' });
    eq(v.hero, '43%');
    eq(v.doseChip, null);
});

test('formatDuration', () => {
    eq(States.formatDuration(0), '0 min');
    eq(States.formatDuration(30000), '< 1 min');
    eq(States.formatDurationShort(0), '');
    eq(States.formatDurationShort(20000), '<1m');
    eq(States.formatDuration(45 * 60000), '45 min');
    eq(States.formatDuration(2 * 3600000), '2 h');
    eq(States.formatDuration(3900000), '1 h 5 min');
    eq(States.formatDurationShort(3900000), '1h05');
});

test('formatDbfs usa sinal de menos tipográfico e esconde piso', () => {
    eq(States.formatDbfs(-34.6), '−35 dBFS');
    eq(States.formatDbfs(-120), '—');
    eq(States.formatDbfs(null), '—');
});

test('rótulos da semana terminam em Hoje', () => {
    // 29/10/2026 é quinta-feira.
    const labels = States.weekdayLabels(new Date(2026, 9, 29));
    eq(labels.join(','), 'Sex,Sáb,Dom,Seg,Ter,Qua,Hoje');
});

test('semana: média só sobre dias acompanhados', () => {
    const today = new Date(2026, 9, 29, 15, 0);
    const since = new Date(2026, 9, 28, 20, 0).getTime();
    const s = States.normalizeStatus({
        measurement_status: 'UNVERIFIED', mascot_state: 'IDLE',
        listening_ms: 2 * 3600000, signal_listening_ms: 2 * 3600000, monitoring_since: since,
        daily_listening_ms: [0, 0, 0, 0, 0, 3600000, 3600000],
        device_listening: [['Chu2 DSP', 'unknown', 5400000], ['Caixa', 'speaker', 1800000], ['Ruim', 'x', 0]],
    });
    const w = States.describeWeek(s, today);
    eq(w.total, '2 h');
    eq(w.dailyAverage, '1 h');
    eq(w.dailyAverageCaption, 'por dia (2 dias acompanhados)');
    eq(w.bars.length, 7);
    eq(w.bars[6].isToday, true);
    eq(w.bars.map(b => b.noData).join(), 'true,true,true,true,true,false,false', 'dias sem dados:');
    eq(w.bars[0].accessible, 'Sex: sem dados');
    eq(w.devices.length, 2, 'dispositivo sem áudio é omitido:');
    eq(w.devices[0].percentLabel, '75%');
    eq(w.devices[1].icon, 'audio-speakers-symbolic');
});

test('semana: dados diários inválidos não quebram', () => {
    const s = States.normalizeStatus({ measurement_status: 'UNVERIFIED', mascot_state: 'IDLE', daily_listening_ms: [1, 2] });
    eq(s.dailyListeningMs, null);
    const w = States.describeWeek(s, new Date(2026, 9, 29));
    eq(w.bars.every(b => b.ms === 0 && b.noData), true, 'sem histórico, tudo sem dados:');
    eq(w.dailyAverageCaption, 'por dia');
    eq(w.coverage.startsWith('Ainda não há histórico'), true, 'cobertura:');
});

test('estado da análise do sinal', () => {
    const [, off] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'IDLE' });
    eq(off.signal.state, 'off');
    const [, live] = view({ measurement_status: 'UNVERIFIED', mascot_state: 'UNVERIFIED', capture_status: 'capturing', signal_analysis: true });
    eq(live.signal.state, 'live');
    eq(live.signal.hint.includes('Não é dBA'), true, 'hint:');
});

test('nível ao vivo: zona e tempo até a referência', () => {
    eq(Math.round(States.hoursToReference(80)), 40);
    eq(Math.round(States.hoursToReference(90)), 4);
    const loud = States.describeLive(90, 5);
    eq(loud.label, '≈ 90 dB(A)');
    eq(loud.zone, 'very-high');
    eq(loud.hint, 'Nesse nível, a referência semanal chegaria em ~4 h.');
    eq(loud.caption, 'Agora no ouvido · ±5 dB');
    eq(States.describeLive(85).zone, 'high');
    eq(States.describeLive(60).zone, 'ok');
    eq(States.describeLive(60).hint, 'Nesse nível, a referência semanal levaria mais de uma semana.');
    eq(States.describeLive(null), null);
});

test('CSS sem margens/paddings negativos (estouram a altura no St)', () => {
    const css = imports.byteArray.toString(
        here.get_parent().get_child('hearphant@gab-simon.github.io').get_child('stylesheet.css').load_contents(null)[1]);
    const bad = css.split('\n').filter(l => /(margin|padding)[^:]*:[^;]*-\d/.test(l));
    eq(bad.length, 0, `linhas: ${bad.join(' | ')}`);
});

test('volume alto agora muda o humor e marca o estado', () => {
    const [, v] = view({ measurement_status: 'ESTIMATED', mascot_state: 'NORMAL', dose_percent: 5, dose_low_percent: 1, dose_high_percent: 15, dose_uncertainty_db: 5, loud_now: true, playing: true });
    eq(v.loud, true);
    eq(v.mood, 'Volume muito alto agora! Que tal baixar um pouco?');
    const [, off] = view(null);
    eq(off.loud, false);
});

print(`\n${count - failures}/${count} testes passaram`);
if (failures > 0)
    imports.system.exit(1);
