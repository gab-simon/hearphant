/* Lógica de apresentação pura: não importa nada do GNOME Shell para poder
 * ser testada com `gjs` fora do Shell. Não calcula dose; só interpreta o que
 * o serviço publica e aplica as salvaguardas da UI (seções 5.4, 6.4 e 9.2). */
'use strict';

var MascotState = Object.freeze({
    UNAVAILABLE: 'UNAVAILABLE', // apenas local: serviço ausente no barramento
    UNVERIFIED: 'UNVERIFIED',
    IDLE: 'IDLE',
    NORMAL: 'NORMAL',
    NOTICE: 'NOTICE',
    WARNING: 'WARNING',
    EXCEEDED: 'EXCEEDED',
    PROTECTING: 'PROTECTING',
    ERROR: 'ERROR',
});

var ProtectionModes = Object.freeze([
    { id: 'inform', label: 'Apenas informar', hint: 'Avisos e orientações, sem mexer no volume' },
    { id: 'ask', label: 'Perguntar antes de reduzir', hint: 'Sugere reduzir e espera a sua escolha' },
    { id: 'auto', label: 'Redução automática', hint: 'Exige um perfil calibrado' },
    { id: 'basic_cap', label: 'Limite percentual básico', hint: 'Teto de volume; não é limite acústico' },
]);

var DEVICE_KINDS = Object.freeze({ headphone: 'Fone de ouvido', speaker: 'Caixa de som', unknown: 'Tipo desconhecido' });

var KIND_CHOICES = Object.freeze([
    { id: 'auto', label: 'Detectar automaticamente' },
    { id: 'headphone', label: 'Fone de ouvido' },
    { id: 'speaker', label: 'Caixa de som' },
]);

/* Ícones simbólicos do tema para cada tipo de saída. */
var DEVICE_ICONS = Object.freeze({
    headphone: 'audio-headphones-symbolic',
    speaker: 'audio-speakers-symbolic',
    unknown: 'audio-card-symbolic',
});

const PHYSICAL_SOURCES = ['CALIBRATED', 'ESTIMATED'];
const MEASUREMENT_STATUSES = ['CALIBRATED', 'ESTIMATED', 'UNVERIFIED', 'UNAVAILABLE'];
const WEEKDAYS = ['Dom', 'Seg', 'Ter', 'Qua', 'Qui', 'Sex', 'Sáb'];
const DAYS_SHOWN = 7;

function _num(v) {
    return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

function _str(v, fallback = '') {
    return typeof v === 'string' && v.length > 0 ? v : fallback;
}

function _nonNegative(v) {
    const n = _num(v);
    return n === null ? null : Math.max(n, 0);
}

/* Estado quando o serviço não está no barramento. */
function unavailableStatus() {
    return normalizeStatus(null);
}

function _normalizeDaily(v) {
    if (!Array.isArray(v) || v.length !== DAYS_SHOWN)
        return null;
    const days = v.map(_nonNegative);
    return days.includes(null) ? null : days;
}

function _normalizeDevices(v) {
    if (!Array.isArray(v))
        return [];
    return v
        .filter(e => Array.isArray(e) && e.length === 3)
        .map(([name, kind, ms]) => ({
            name: _str(name, 'Saída sem nome'),
            kind: kind in DEVICE_KINDS ? kind : 'unknown',
            ms: _nonNegative(ms) ?? 0,
        }))
        .filter(d => d.ms > 0);
}

/* Converte o dicionário recebido via D-Bus (já desempacotado) em um objeto
 * com tipos garantidos. `raw === null` significa serviço ausente. */
function normalizeStatus(raw) {
    const serviceAbsent = raw === null || typeof raw !== 'object';
    if (serviceAbsent)
        raw = {};

    let measurementStatus = _str(raw.measurement_status, 'UNAVAILABLE');
    if (!MEASUREMENT_STATUSES.includes(measurementStatus))
        measurementStatus = 'UNAVAILABLE';

    const physical = PHYSICAL_SOURCES.includes(measurementStatus);

    // Nunca exibir dose sem fonte acústica (calibrada ou estimada com faixa),
    // mesmo que o serviço envie o campo por engano.
    let dosePercent = physical ? _num(raw.dose_percent) : null;
    if (measurementStatus === 'ESTIMATED' && (_num(raw.dose_low_percent) === null || _num(raw.dose_high_percent) === null))
        dosePercent = null;
    const estimated = measurementStatus === 'ESTIMATED' && dosePercent !== null;
    let doseLow = estimated ? _num(raw.dose_low_percent) : null;
    let doseHigh = estimated ? _num(raw.dose_high_percent) : null;
    // Estimativa sem faixa não é exibida como número (seção 9.2).
    const estimateIncomplete = estimated && (doseLow === null || doseHigh === null);
    if (estimateIncomplete)
        doseLow = doseHigh = null;
    const doseUncertaintyDb = estimated ? _num(raw.dose_uncertainty_db) : null;

    let mascotState = _str(raw.mascot_state, MascotState.ERROR);
    if (serviceAbsent)
        mascotState = MascotState.UNAVAILABLE;
    else if (!(mascotState in MascotState) || mascotState === MascotState.UNAVAILABLE)
        mascotState = MascotState.ERROR;

    // Estados que dependem de dose não podem aparecer sem dose conhecida:
    // o verde de NORMAL não deve sugerir "audição segura" (seção 5.3).
    const doseStates = [MascotState.NORMAL, MascotState.NOTICE, MascotState.WARNING, MascotState.EXCEEDED];
    if (doseStates.includes(mascotState) && dosePercent === null)
        mascotState = measurementStatus === 'UNAVAILABLE' ? MascotState.ERROR : MascotState.UNVERIFIED;

    const kind = _str(raw.device_kind, 'unknown');
    const coverageFraction = _num(raw.coverage_fraction);
    const monitoredFraction = _num(raw.monitored_fraction);
    const captureStatus = ['off', 'waiting', 'capturing', 'error'].includes(raw.capture_status)
        ? raw.capture_status : 'off';

    return {
        mascotState,
        measurementStatus,
        dosePercent,
        doseLow,
        doseHigh,
        doseUncertaintyDb,
        estimateAvailable: raw.estimate_available === true,
        estimateProfile: _str(raw.estimate_profile) || null,
        estimateBasis: _str(raw.estimate_basis) || null,
        estimateReason: _str(raw.estimate_reason) || null,
        estimateUncertaintyDb: _num(raw.estimate_uncertainty_db),
        kindPresumed: raw.kind_presumed === true,
        kindOverride: ['auto', 'headphone', 'speaker'].includes(raw.device_kind_override) ? raw.device_kind_override : 'auto',
        loudNow: raw.loud_now === true,
        coverageStatus: _str(raw.coverage_status, 'missing'),
        coverageFraction: coverageFraction === null ? null : Math.min(Math.max(coverageFraction, 0), 1),
        windowHours: _num(raw.window_hours) ?? 168,
        referenceMode: _str(raw.reference_mode, 'adult'),
        deviceName: _str(raw.device_name),
        deviceKind: kind in DEVICE_KINDS ? kind : 'unknown',
        volumePercent: _num(raw.volume_percent),
        muted: raw.muted === true,
        playing: raw.playing === true,
        streamActive: raw.stream_active === true,
        protectionMode: ProtectionModes.some(m => m.id === raw.protection_mode) ? raw.protection_mode : null,
        isDemo: raw.is_demo === true,
        // Atividade digital (não é exposição acústica).
        signalAnalysis: raw.signal_analysis === true,
        captureStatus,
        activitySource: raw.activity_source === 'signal' ? 'signal' : 'stream',
        listeningMs: _nonNegative(raw.listening_ms),
        signalListeningMs: _nonNegative(raw.signal_listening_ms) ?? 0,
        monitoredMs: _nonNegative(raw.monitored_ms),
        monitoredFraction: monitoredFraction === null ? null : Math.min(Math.max(monitoredFraction, 0), 1),
        monitoringSince: _num(raw.monitoring_since),
        signalMeanDbfs: _num(raw.signal_mean_dbfs),
        dailyListeningMs: _normalizeDaily(raw.daily_listening_ms),
        deviceListening: _normalizeDevices(raw.device_listening),
    };
}

/* "6 h 20 min", "45 min", "< 1 min", "0 min". */
function formatDuration(ms) {
    if (ms === null || ms === undefined)
        return '—';
    const totalMin = Math.floor(ms / 60000);
    if (ms > 0 && totalMin === 0)
        return '< 1 min';
    const h = Math.floor(totalMin / 60);
    const m = totalMin % 60;
    if (h === 0)
        return `${m} min`;
    return m === 0 ? `${h} h` : `${h} h ${m} min`;
}

/* Versão curta para rótulos de barras: "1h05", "45m", "<1m"; vazio sem áudio. */
function formatDurationShort(ms) {
    if (!ms)
        return '';
    const totalMin = Math.round(ms / 60000);
    if (totalMin === 0)
        return '<1m';
    const h = Math.floor(totalMin / 60);
    const m = totalMin % 60;
    return h === 0 ? `${m}m` : `${h}h${String(m).padStart(2, '0')}`;
}

function formatDbfs(value) {
    if (value === null || !Number.isFinite(value) || value <= -120)
        return '—';
    return `${Math.round(value).toLocaleString('pt-BR').replace('-', '−')} dBFS`;
}

function _formatPercent(value) {
    // Não truncar excedentes: 125% é exibido como 125% (seção 6.5).
    if (value > 0 && value < 0.5)
        return '< 1%';
    return `${Math.round(value).toLocaleString('pt-BR')}%`;
}

/* Horas até 100% da referência adulta a nível constante (seção 8.3). */
function hoursToReference(laeq) {
    return 40 * 10 ** ((80 - laeq) / 10);
}

/* Escala do medidor ao vivo em dB(A) estimado. */
var LIVE_SCALE = Object.freeze({ min: 40, max: 110, markers: [80, 90] });

/* Nível estimado agora: rótulo, zona de cor e tradução em tempo. Zonas pelo
 * tempo até a referência semanal: ≥ 80 dB(A) → até 40 h; ≥ 90 → até 4 h. */
function describeLive(laeq, uncertaintyDb = null) {
    if (laeq === null || !Number.isFinite(laeq))
        return null;
    const hours = hoursToReference(laeq);
    const hint = hours >= 168
        ? 'Nesse nível, a referência semanal levaria mais de uma semana.'
        : `Nesse nível, a referência semanal chegaria em ~${formatDuration(hours * 3600000)}.`;
    return {
        label: formatDba(laeq),
        zone: laeq >= 90 ? 'very-high' : laeq >= 80 ? 'high' : 'ok',
        hint,
        caption: uncertaintyDb ? `Agora no ouvido · ±${Math.round(uncertaintyDb)} dB` : 'Agora no ouvido',
    };
}

function formatDba(value) {
    return value === null || !Number.isFinite(value) ? '—' : `≈ ${Math.round(value)} dB(A)`;
}

/* Rótulos dos 7 dias terminando em `today` (Date local). */
function weekdayLabels(today) {
    const labels = [];
    for (let i = DAYS_SHOWN - 1; i >= 0; i--) {
        const d = new Date(today.getFullYear(), today.getMonth(), today.getDate() - i);
        labels.push(i === 0 ? 'Hoje' : WEEKDAYS[d.getDay()]);
    }
    return labels;
}

function _formatDateTime(ms) {
    const d = new Date(ms);
    const pad = n => String(n).padStart(2, '0');
    return `${pad(d.getDate())}/${pad(d.getMonth() + 1)} às ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const DETAILS = {
    UNAVAILABLE: 'O serviço HearPhant não está em execução. A Ellie não está acompanhando o áudio.',
    UNVERIFIED: 'A Ellie está acompanhando o áudio, mas ainda não consegue calcular a exposição real deste dispositivo.',
    IDLE: 'Nenhuma reprodução de áudio detectada no momento.',
    NORMAL: 'Exposição acompanhada.',
    NOTICE: 'Sua exposição acumulada está aumentando. Que tal diminuir um pouco o volume?',
    WARNING: 'Sua exposição acumulada está alta. Considere reduzir o volume ou fazer uma pausa.',
    EXCEEDED: 'Você ultrapassou a referência semanal para este perfil. Continuar ouvindo no mesmo nível aumenta o risco.',
    PROTECTING: 'A Ellie reduziu o volume deste dispositivo. Confira se ele ficou confortável.',
    ERROR: 'Não consegui monitorar esta saída de áudio. Ajuste o volume diretamente no equipamento.',
};

const SHORT_STATE = {
    UNAVAILABLE: 'serviço indisponível',
    UNVERIFIED: 'dose indisponível, dispositivo não calibrado',
    IDLE: 'sem reprodução',
    NORMAL: 'exposição acompanhada',
    NOTICE: 'exposição aumentando',
    WARNING: 'exposição alta',
    EXCEEDED: 'referência semanal excedida',
    PROTECTING: 'volume reduzido',
    ERROR: 'erro de monitoramento',
};

const SOURCE_LABELS = {
    CALIBRATED: 'Fonte calibrada',
    ESTIMATED: 'Estimativa',
    UNVERIFIED: 'Fonte não verificada',
    UNAVAILABLE: 'Sem monitoramento',
};

const CALIBRATION_LABELS = {
    CALIBRATED: 'Calibrado',
    ESTIMATED: 'Estimado',
    UNVERIFIED: 'Não calibrado',
    UNAVAILABLE: 'Sem monitoramento',
};

/* "faixa 2% a 21%" ou "faixa abaixo de 1%". */
function _rangeText(s) {
    if (s.doseLow === null)
        return 'sem faixa';
    if (s.doseHigh < 1)
        return 'faixa abaixo de 1%';
    return `faixa ${_formatPercent(s.doseLow)} a ${_formatPercent(s.doseHigh)}`;
}

function _doseChip(s, available, hasDose, estimated) {
    if (!available || s.mascotState === MascotState.ERROR)
        return null;
    if (estimated)
        return null; // a faixa já vai na legenda do número
    if (hasDose)
        return null;
    if (s.estimateAvailable && !s.signalAnalysis)
        return 'Ative a análise do sinal para estimar a exposição  →';
    if (s.estimateAvailable)
        return 'Estimando… a dose aparece com o primeiro áudio';
    return s.estimateReason ?? 'Dose indisponível para esta saída';
}

function _deviceCaption(s, available) {
    if (!available)
        return '';
    if (s.estimateAvailable)
        return `Estimativa ±${Math.round(s.estimateUncertaintyDb ?? 0)} dB`;
    return s.kindOverride === 'speaker' || s.deviceKind === 'speaker' ? 'Sem estimativa (caixa)' : CALIBRATION_LABELS[s.measurementStatus];
}

function _calibrationLine(s) {
    if (s.estimateAvailable) {
        let line = `Estimativa automática: ${s.estimateProfile} (±${Math.round(s.estimateUncertaintyDb ?? 0)} dB).`;
        if (s.kindPresumed)
            line += ' Tipo presumido como fone; ajuste abaixo se for caixa de som.';
        if (s.estimateBasis)
            line += ` ${s.estimateBasis}`;
        return line;
    }
    return s.estimateReason ?? 'Sem estimativa para esta saída.';
}

/* Textos e parâmetros visuais da tela principal. */
function describe(status) {
    const s = status;
    const hasDose = s.dosePercent !== null;
    const estimated = hasDose && s.measurementStatus === 'ESTIMATED';
    const available = s.mascotState !== MascotState.UNAVAILABLE;
    const approximate = s.listeningMs !== null && s.signalListeningMs < s.listeningMs;

    // Herói: dose quando existe; senão tempo de escuta, que é medido de fato.
    let hero, heroCaption, headline;
    if (!available) {
        hero = '—';
        heroCaption = 'Serviço indisponível';
        headline = 'Serviço indisponível';
    } else if (hasDose) {
        hero = estimated ? `≈ ${_formatPercent(s.dosePercent)}` : _formatPercent(s.dosePercent);
        heroCaption = estimated
            ? `da referência semanal · estimativa, ${_rangeText(s)}`
            : 'da referência semanal';
        headline = `${hero} da referência`;
    } else if (s.mascotState === MascotState.ERROR) {
        hero = '—';
        heroCaption = 'Monitoramento indisponível';
        headline = 'Monitoramento indisponível';
    } else {
        hero = formatDuration(s.listeningMs ?? 0);
        heroCaption = approximate ? 'de áudio em 7 dias (aproximado)' : 'de áudio nos últimos 7 dias';
        headline = 'Dose indisponível: calibre seu dispositivo';
    }

    let mood;
    const alerting = [MascotState.NOTICE, MascotState.WARNING, MascotState.EXCEEDED,
        MascotState.ERROR, MascotState.PROTECTING].includes(s.mascotState);
    if (!available)
        mood = 'Inicie o serviço para a Ellie acompanhar';
    else if (s.loudNow)
        mood = 'Volume muito alto agora! Que tal baixar um pouco?';
    else if (alerting || (hasDose && !estimated))
        mood = DETAILS[s.mascotState];
    else
        mood = s.playing ? 'Ouvindo agora' : 'Em silêncio';

    const days = Math.round(s.windowHours / 24);
    let source = SOURCE_LABELS[s.measurementStatus];
    if (estimated && s.doseUncertaintyDb !== null)
        source += ` automática (±${Math.round(s.doseUncertaintyDb)} dB)`;
    const sourceLine = hasDose ? `Últimos ${days} dias · ${source}` : source;

    let deviceLine = null;
    if (s.deviceName)
        deviceLine = `Dispositivo: ${s.deviceName} (${DEVICE_KINDS[s.deviceKind].toLowerCase()})`;

    let volumeLine = null;
    if (s.volumePercent !== null) {
        volumeLine = `Volume do sistema: ${Math.round(s.volumePercent)}%${s.muted ? ' · mudo' : ''}`;
        volumeLine += ' (valor digital, não é dBA)';
    }

    let coverageLine = null;
    if (available) {
        coverageLine = 'Somente o som deste computador; não inclui exposição fora dele.';
        if (hasDose && s.coverageFraction !== null && s.coverageFraction < 1)
            coverageLine = `Cobertura: ${_formatPercent(s.coverageFraction * 100)} da semana. ${coverageLine}`;
    }

    const protection = ProtectionModes.find(m => m.id === s.protectionMode);

    let accessibleName = `HearPhant: ${hasDose ? headline : SHORT_STATE[s.mascotState]}`;
    if (available && !hasDose && s.listeningMs !== null)
        accessibleName += `; ${hero} ${heroCaption}`;
    if (s.isDemo)
        accessibleName = `${accessibleName} (demonstração, dados fictícios)`;

    let signal;
    if (!available)
        signal = { state: 'off', label: '—', hint: '' };
    else if (s.captureStatus === 'capturing')
        signal = { state: 'live', label: '', hint: 'Nível do conteúdo, antes do volume. Não é dBA.' };
    else if (s.captureStatus === 'waiting')
        signal = { state: 'idle', label: 'Aguardando áudio', hint: 'A análise começa quando algo tocar.' };
    else if (s.captureStatus === 'error')
        signal = { state: 'error', label: 'Falha na análise', hint: 'Usando os aplicativos abertos como aproximação.' };
    else
        signal = { state: 'off', label: 'Análise desligada', hint: 'Ative em Configurações para medir o sinal.' };

    return {
        hero,
        heroCaption,
        headline,
        mood,
        doseChip: _doseChip(s, available, hasDose, estimated),
        // O chip vira botão para Configurações quando há algo a ativar.
        doseChipAction: available && !hasDose && s.estimateAvailable && !s.signalAnalysis,
        detail: DETAILS[s.mascotState],
        sourceLine,
        deviceLine,
        volumeLine,
        coverageLine,
        volumeTile: s.volumePercent === null ? '—' : `${Math.round(s.volumePercent)}%`,
        volumeTileCaption: s.muted ? 'mudo · valor digital' : 'valor digital, não é dBA',
        deviceTile: s.deviceName || '—',
        deviceTileCaption: _deviceCaption(s, available),
        deviceIcon: DEVICE_ICONS[s.deviceKind],
        calibrated: hasDose && !estimated,
        estimated,
        rangeText: estimated ? _rangeText(s) : null,
        calibrationLine: _calibrationLine(s),
        signal,
        protectionLabel: protection ? protection.label : null,
        accessibleName,
        // null = anel tracejado ("não sabemos"), nunca 0%.
        ringFraction: hasDose ? Math.max(s.dosePercent, 0) / 100 : null,
        showQuestionBadge: s.mascotState === MascotState.UNVERIFIED,
        loud: available && s.loudNow,
        estimateShort: s.estimateAvailable
            ? `${s.estimateProfile} · ±${Math.round(s.estimateUncertaintyDb ?? 0)} dB`
            : 'Sem estimativa',
        styleClass: `hearphant-state-${s.mascotState.toLowerCase()}`,
    };
}

/* Modelo da tela "Escuta da semana". */
function describeWeek(status, today = new Date()) {
    const s = status;
    const daily = s.dailyListeningMs ?? new Array(DAYS_SHOWN).fill(0);
    const total = s.listeningMs ?? daily.reduce((a, b) => a + b, 0);
    const labels = weekdayLabels(today);
    const approximate = s.listeningMs !== null && s.signalListeningMs < s.listeningMs;

    // Média só sobre os dias efetivamente acompanhados: dia sem dados não é 0.
    let observedDays = s.monitoringSince === null ? 0 : DAYS_SHOWN;
    if (s.monitoringSince !== null) {
        const start = new Date(s.monitoringSince);
        const startDay = new Date(start.getFullYear(), start.getMonth(), start.getDate());
        const todayDay = new Date(today.getFullYear(), today.getMonth(), today.getDate());
        observedDays = Math.min(DAYS_SHOWN, Math.max(1, Math.round((todayDay - startDay) / 86400000) + 1));
    }

    const totalDevices = s.deviceListening.reduce((a, d) => a + d.ms, 0);
    const devices = s.deviceListening.map(d => ({
        name: d.name,
        icon: DEVICE_ICONS[d.kind],
        kindLabel: DEVICE_KINDS[d.kind],
        fraction: totalDevices > 0 ? d.ms / totalDevices : 0,
        percentLabel: `${Math.round(totalDevices > 0 ? 100 * d.ms / totalDevices : 0)}%`,
        durationLabel: formatDuration(d.ms),
    }));

    let coverage;
    if (s.monitoringSince === null)
        coverage = 'Ainda não há histórico. A Ellie começa a contar a partir de agora.';
    else
        coverage = `Acompanhando desde ${_formatDateTime(s.monitoringSince)}. Períodos sem o serviço ficam sem dados; não contam como silêncio.`;

    return {
        bars: daily.map((ms, i) => {
            // Dias antes do início do acompanhamento não têm dados.
            const noData = DAYS_SHOWN - 1 - i >= observedDays && ms === 0;
            return {
                ms,
                label: labels[i],
                valueLabel: noData ? '' : formatDurationShort(ms),
                isToday: i === DAYS_SHOWN - 1,
                noData,
                accessible: `${labels[i]}: ${noData ? 'sem dados' : formatDuration(ms)}`,
            };
        }),
        total: formatDuration(total),
        totalCaption: approximate ? 'nos últimos 7 dias · aproximado' : 'nos últimos 7 dias',
        dailyAverage: formatDuration(total / Math.max(observedDays, 1)),
        dailyAverageCaption: observedDays > 0 && observedDays < DAYS_SHOWN
            ? `por dia (${observedDays} ${observedDays === 1 ? 'dia acompanhado' : 'dias acompanhados'})`
            : 'por dia',
        meanSignal: formatDbfs(s.signalMeanDbfs),
        meanSignalCaption: 'sinal médio do conteúdo · não é dBA',
        devices,
        coverage,
        sourceNote: s.activitySource === 'signal'
            ? 'Tempo com áudio medido pelo sinal: conta qualquer som audível, mesmo baixinho.'
            : 'Tempo aproximado pelos aplicativos com áudio aberto. Ative a análise do sinal para mais precisão.',
    };
}
