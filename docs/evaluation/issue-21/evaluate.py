"""Offline, model-independent meeting evaluation preparation and latency reporting."""
import argparse
import json
import math
from pathlib import Path


def fixture(minutes):
    rows = []
    for i in range(minutes):
        rows.append({'id': f'u{i:03}', 'seconds': i * 60,
                     'text': '雑音。Project Hoshiの進捗を確認します。API latency is unchanged.'})
    rows[1]['text'] = 'Project Hoshiの公開日は12月1日と仮置きします。'
    rows[-2]['text'] = '12月1日公開案は撤回します。公開日と担当は未定です。'
    rows[-1]['text'] = '決定事項は負荷試験を先に行うことです。期限と担当はまだ決めません。'
    return {'duration_seconds': minutes * 60, 'synthetic': True, 'utterances': rows,
            'gold': [{'claim': '負荷試験を先に行う', 'evidence': [rows[-1]['id']]}],
            'forbidden': ['12月1日公開を確定', '担当者の断定', '期限の断定'],
            'question': '公開日は決まりましたか？',
            'answer': {'text': '未定。12月1日案は撤回された。', 'evidence': [rows[-2]['id']]}}


def prompt(rows):
    return ('発話だけを根拠に要約し、撤回を反映してください。担当・期限の不明はnull。'
            'JSONでclaims:[{text,evidence:[発話ID]}]とanswer:{text,evidence:[発話ID]}を返す。'
            '質問:公開日は決まりましたか？\n' + '\n'.join(f"{r['id']}: {r['text']}" for r in rows))


def percentile(values, p):
    values = sorted(values)
    return values[max(0, math.ceil(len(values) * p) - 1)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--metrics', type=Path, help='JSON list of nonnegative latency samples in milliseconds')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    for minutes in (10, 60):
        data = fixture(minutes)
        (args.output / f'{minutes}m.json').write_text(json.dumps(data, ensure_ascii=False, indent=2))
        (args.output / f'{minutes}m-full.txt').write_text(prompt(data['utterances']))
        chunks = [prompt(data['utterances'][i:i + 5]) for i in range(0, minutes, 5)]
        (args.output / f'{minutes}m-chunks.json').write_text(json.dumps(chunks, ensure_ascii=False, indent=2))
    if args.metrics:
        samples = json.loads(args.metrics.read_text())
        if not isinstance(samples, list) or not samples or any(type(x) not in (int, float) or not math.isfinite(x) or x < 0 for x in samples):
            raise ValueError('metrics must be a nonempty list of finite nonnegative numbers')
        print(json.dumps({'n': len(samples), 'p50_ms': percentile(samples, .5), 'p95_ms': percentile(samples, .95)}))


if __name__ == '__main__':
    main()
