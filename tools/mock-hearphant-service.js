#!/usr/bin/env gjs
/* Serviço SIMULADO de io.github.gab_simon.HearPhant1 para desenvolver as interfaces
 * sem o daemon real. Todos os dados são fictícios e marcados com is_demo=true.
 *
 * Uso: gjs tools/mock-hearphant-service.js [cenário]
 * Cenários: unverified (padrão), idle, normal, notice, warning, estimated,
 *           exceeded, protecting, protection-failed, loud, error, cycle */
'use strict';

const { Gio, GLib } = imports.gi;
const ByteArray = imports.byteArray;

const BUS_NAME = 'io.github.gab_simon.HearPhant1';
const OBJECT_PATH = '/io/github/gab_simon/HearPhant1';
const CYCLE_SECONDS = 5;

const scriptDir = Gio.File.new_for_path(imports.system.programPath).get_parent();
const xmlFile = scriptDir.get_parent().get_child('dbus').get_child(`${BUS_NAME}.xml`);
const [, xmlBytes] = xmlFile.load_contents(null);
const IFACE_XML = ByteArray.toString(xmlBytes);

const HOUR = 3600000;
const BASE = {
    api_version: 3,
    window_hours: 168,
    reference_mode: 'adult',
    device_name: 'Fone simulado',
    device_kind: 'headphone',
    volume_percent: 35,
    muted: false,
    playing: true,
    protection_mode: 'inform',
    is_demo: true,
    signal_analysis: true,
    capture_status: 'capturing',
    activity_source: 'signal',
    stream_active: true,
    listening_ms: 22.4 * HOUR,
    signal_listening_ms: 22.4 * HOUR,
    monitored_ms: 168 * HOUR,
    monitored_fraction: 1,
    monitoring_since: Date.now() - 168 * HOUR,
    signal_mean_dbfs: -21.5,
    daily_listening_ms: [2.1, 3.4, 4.8, 2.6, 4.2, 3.9, 1.4].map(h => Math.round(h * HOUR)),
    device_listening: [['Fone simulado', 'headphone', Math.round(15.2 * HOUR)], ['Caixa simulada', 'speaker', Math.round(5.4 * HOUR)], ['Monitor HDMI', 'unknown', Math.round(1.8 * HOUR)]],
};

const SCENARIOS = {
    unverified: { mascot_state: 'UNVERIFIED', measurement_status: 'UNVERIFIED', coverage_status: 'unverified' },
    idle: { mascot_state: 'IDLE', measurement_status: 'UNVERIFIED', coverage_status: 'unverified', playing: false },
    normal: { mascot_state: 'NORMAL', measurement_status: 'CALIBRATED', coverage_status: 'observed', coverage_fraction: 1, dose_percent: 23 },
    notice: { mascot_state: 'NOTICE', measurement_status: 'CALIBRATED', coverage_status: 'observed', coverage_fraction: 0.8, dose_percent: 56 },
    estimated: { mascot_state: 'NOTICE', measurement_status: 'ESTIMATED', coverage_status: 'observed', coverage_fraction: 1, dose_percent: 61, dose_low_percent: 19.3, dose_high_percent: 192.9, dose_uncertainty_db: 5, estimate_available: true, estimate_profile: 'Fone simulado', estimate_uncertainty_db: 5, estimate_basis: 'Perfil fictício.' },
    warning: { mascot_state: 'WARNING', measurement_status: 'CALIBRATED', coverage_status: 'observed', coverage_fraction: 1, dose_percent: 87 },
    exceeded: { mascot_state: 'EXCEEDED', measurement_status: 'CALIBRATED', coverage_status: 'observed', coverage_fraction: 1, dose_percent: 132 },
    protecting: { mascot_state: 'PROTECTING', measurement_status: 'CALIBRATED', coverage_status: 'observed', coverage_fraction: 1, dose_percent: 134, volume_percent: 20, protection_mode: 'auto' },
    'protection-failed': { mascot_state: 'EXCEEDED', measurement_status: 'CALIBRATED', coverage_status: 'observed', coverage_fraction: 1, dose_percent: 134, protection_mode: 'auto' },
    loud: { mascot_state: 'NORMAL', measurement_status: 'ESTIMATED', coverage_status: 'observed', coverage_fraction: 0.4, dose_percent: 34, dose_low_percent: 10.8, dose_high_percent: 107.5, dose_uncertainty_db: 5, estimate_available: true, estimate_profile: 'Fone simulado', estimate_uncertainty_db: 5, estimate_basis: 'Perfil fictício.', loud_now: true },
    error: { mascot_state: 'ERROR', measurement_status: 'UNAVAILABLE', coverage_status: 'missing', device_name: '', volume_percent: undefined },
};
const CYCLE = ['unverified', 'idle', 'normal', 'notice', 'estimated', 'warning', 'exceeded', 'protecting', 'protection-failed', 'loud', 'error'];

const UINT_KEYS = new Set(['api_version', 'window_hours']);
const UINT64_KEYS = new Set(['listening_ms', 'signal_listening_ms', 'monitored_ms']);

function pack(obj) {
    const out = {};
    for (const [key, value] of Object.entries(obj)) {
        if (value === undefined)
            continue;
        if (typeof value === 'boolean')
            out[key] = new GLib.Variant('b', value);
        else if (typeof value === 'string')
            out[key] = new GLib.Variant('s', value);
        else if (UINT_KEYS.has(key))
            out[key] = new GLib.Variant('u', value);
        else if (UINT64_KEYS.has(key))
            out[key] = new GLib.Variant('t', value);
        else if (key === 'monitoring_since')
            out[key] = new GLib.Variant('x', value);
        else if (key === 'daily_listening_ms')
            out[key] = new GLib.Variant('at', value);
        else if (key === 'device_listening')
            out[key] = new GLib.Variant('a(sst)', value);
        else
            out[key] = new GLib.Variant('d', value);
    }
    return out;
}

const requested = ARGV[0] ?? 'unverified';
if (requested !== 'cycle' && !(requested in SCENARIOS)) {
    printerr(`Cenário desconhecido: ${requested}\nDisponíveis: ${Object.keys(SCENARIOS).join(', ')}, cycle`);
    imports.system.exit(2);
}

let scenario = requested === 'cycle' ? CYCLE[0] : requested;
let protectionMode = BASE.protection_mode;
let signalAnalysis = BASE.signal_analysis;

function currentStatus() {
    const status = { ...BASE, protection_mode: protectionMode, ...SCENARIOS[scenario], signal_analysis: signalAnalysis };
    if (!signalAnalysis)
        Object.assign(status, { capture_status: 'off', activity_source: 'stream', signal_listening_ms: 0, signal_mean_dbfs: undefined });
    return status;
}

const service = {
    get ApiVersion() {
        return 1;
    },

    GetStatus() {
        return pack(currentStatus());
    },

    GetExposure(windowHours) {
        const s = currentStatus();
        return pack({
            window_hours: windowHours,
            measurement_status: s.measurement_status,
            coverage_status: s.coverage_status,
            coverage_fraction: s.coverage_fraction,
            dose_percent: s.dose_percent,
            is_demo: true,
        });
    },

    ListDevices() {
        return [pack({ id: 'mock-0', display_name: BASE.device_name, kind: BASE.device_kind, is_demo: true })];
    },

    GetLevel() {
        if (!signalAnalysis || !currentStatus().playing)
            return [false, -120, -120, false, -1];
        const t = Date.now() / 1000;
        const rms = -24 + 6 * Math.sin(t * 1.7) + 3 * Math.sin(t * 5.3);
        return [true, rms, rms + 9, true, currentStatus().loud_now ? 94 + rms / 10 : rms + 95];
    },

    SetDeviceKind(kind) {
        print(`tipo da saída → ${kind} (simulado)`);
        return [true, ''];
    },

    SetSignalAnalysis(enabled) {
        signalAnalysis = enabled;
        print(`análise do sinal → ${enabled}`);
        emitStatus();
        return [true, ''];
    },

    DeleteHistory(_from, _to) {
        print('histórico apagado (simulado)');
        return 42;
    },

    SetProtectionMode(mode) {
        if (!['inform', 'ask', 'auto', 'basic_cap'].includes(mode))
            return [false, `Modo desconhecido: ${mode}`];
        if (mode === 'auto' && currentStatus().measurement_status !== 'CALIBRATED')
            return [false, 'Redução automática exige um perfil calibrado para esta saída.'];
        protectionMode = mode;
        print(`modo de proteção → ${mode}`);
        emitStatus();
        return [true, ''];
    },
};

const exported = Gio.DBusExportedObject.wrapJSObject(IFACE_XML, service);

function emitStatus() {
    exported.emit_signal('StatusChanged', new GLib.Variant('(a{sv})', [service.GetStatus()]));
}

function emitLoudAlert() {
    exported.emit_signal('AlertRaised', new GLib.Variant('(a{sv})', [pack({
        alert_id: 'loud-now',
        severity: 'loud',
        title: 'Volume muito alto: ≈ 94 dB(A) (demonstração)',
        body: 'A Ellie estima que o som no seu ouvido está muito alto. Nesse nível, a referência semanal seria atingida em cerca de 2 h. Que tal baixar o volume?',
    })]));
}

function enterScenario(name) {
    scenario = name;
    print(`cenário → ${name}`);
    emitStatus();

    if (name === 'loud') {
        // Como no daemon real, o aviso vem depois de alguns segundos de som alto.
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, 1500, () => {
            emitLoudAlert();
            return GLib.SOURCE_REMOVE;
        });
    } else if (name === 'exceeded') {
        exported.emit_signal('AlertRaised', new GLib.Variant('(a{sv})', [pack({
            alert_id: 'demo-exceeded',
            severity: 'exceeded',
            title: 'Referência semanal excedida (demonstração)',
            body: 'Você ultrapassou a referência semanal para este perfil. Continuar ouvindo no mesmo nível aumenta o risco.',
        })]));
    } else if (name === 'protecting' || name === 'protection-failed') {
        const verified = name === 'protecting';
        exported.emit_signal('ProtectionApplied', new GLib.Variant('(a{sv})', [pack({
            device_name: BASE.device_name,
            requested_gain: 0.2,
            actual_gain: verified ? 0.2 : 0.35,
            verified,
            message: verified ? '' : 'O volume não mudou após o comando (demonstração).',
        })]));
    }
}

const loop = new GLib.MainLoop(null, false);

Gio.bus_own_name(
    Gio.BusType.SESSION,
    BUS_NAME,
    Gio.BusNameOwnerFlags.NONE,
    connection => exported.export(connection, OBJECT_PATH),
    () => {
        print(`${BUS_NAME} simulado em ${OBJECT_PATH} (dados fictícios)`);
        enterScenario(scenario);
    },
    () => {
        printerr(`Não foi possível obter ${BUS_NAME}; outro serviço já está em execução?`);
        loop.quit();
    });

if (requested === 'cycle') {
    let index = 0;
    GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, CYCLE_SECONDS, () => {
        index = (index + 1) % CYCLE.length;
        enterScenario(CYCLE[index]);
        return GLib.SOURCE_CONTINUE;
    });
}

GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, 2 /* SIGINT */, () => {
    loop.quit();
    return GLib.SOURCE_REMOVE;
});

loop.run();
