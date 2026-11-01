//! Table 9 of the DjVu v3 specification (Appendix 3, the Z'-Coder): the 251 probability states, k = 0 to 250, each with the
//! amount the interval shrinks by (delta), the threshold above which an MPS moves the state (theta), and the next state after an
//! MPS (mu) and after an LPS (lambda).
//!
//! The table in the specification has no rows above 250. Every row was read from the pages of the official file (61 to 64):
//! the text layer of that file gave 234 rows whole and exactly as below, and the other 17 were read from the picture of the page
//! (the printed table has misprinted glyphs, an `O` for `0`, an `l` for `1`, an `S`, `Y`, `?` or `½` for a hex digit, which is
//! why a program reading the text alone cannot be trusted with it), and the whole table was compared with a second copy of it
//! (the MIT crate djvu-rs) without a difference.

use crate::zp::State;

pub(crate) const STATES: [State; 251] = [
    State {
        delta: 0x8000,
        theta: 0x0000,
        mps_next: 84,
        lps_next: 145,
    }, // 0
    State {
        delta: 0x8000,
        theta: 0x0000,
        mps_next: 3,
        lps_next: 4,
    }, // 1
    State {
        delta: 0x8000,
        theta: 0x0000,
        mps_next: 4,
        lps_next: 3,
    }, // 2
    State {
        delta: 0x6BBD,
        theta: 0x10A5,
        mps_next: 5,
        lps_next: 1,
    }, // 3
    State {
        delta: 0x6BBD,
        theta: 0x10A5,
        mps_next: 6,
        lps_next: 2,
    }, // 4
    State {
        delta: 0x5D45,
        theta: 0x1F28,
        mps_next: 7,
        lps_next: 3,
    }, // 5
    State {
        delta: 0x5D45,
        theta: 0x1F28,
        mps_next: 8,
        lps_next: 4,
    }, // 6
    State {
        delta: 0x51B9,
        theta: 0x2BD3,
        mps_next: 9,
        lps_next: 5,
    }, // 7
    State {
        delta: 0x51B9,
        theta: 0x2BD3,
        mps_next: 10,
        lps_next: 6,
    }, // 8
    State {
        delta: 0x4813,
        theta: 0x36E3,
        mps_next: 11,
        lps_next: 7,
    }, // 9
    State {
        delta: 0x4813,
        theta: 0x36E3,
        mps_next: 12,
        lps_next: 8,
    }, // 10
    State {
        delta: 0x3FD5,
        theta: 0x408C,
        mps_next: 13,
        lps_next: 9,
    }, // 11
    State {
        delta: 0x3FD5,
        theta: 0x408C,
        mps_next: 14,
        lps_next: 10,
    }, // 12
    State {
        delta: 0x38B1,
        theta: 0x48FD,
        mps_next: 15,
        lps_next: 11,
    }, // 13
    State {
        delta: 0x38B1,
        theta: 0x48FD,
        mps_next: 16,
        lps_next: 12,
    }, // 14
    State {
        delta: 0x3275,
        theta: 0x505D,
        mps_next: 17,
        lps_next: 13,
    }, // 15
    State {
        delta: 0x3275,
        theta: 0x505D,
        mps_next: 18,
        lps_next: 14,
    }, // 16
    State {
        delta: 0x2CFD,
        theta: 0x56D0,
        mps_next: 19,
        lps_next: 15,
    }, // 17
    State {
        delta: 0x2CFD,
        theta: 0x56D0,
        mps_next: 20,
        lps_next: 16,
    }, // 18
    State {
        delta: 0x2825,
        theta: 0x5C71,
        mps_next: 21,
        lps_next: 17,
    }, // 19
    State {
        delta: 0x2825,
        theta: 0x5C71,
        mps_next: 22,
        lps_next: 18,
    }, // 20
    State {
        delta: 0x23AB,
        theta: 0x615B,
        mps_next: 23,
        lps_next: 19,
    }, // 21
    State {
        delta: 0x23AB,
        theta: 0x615B,
        mps_next: 24,
        lps_next: 20,
    }, // 22
    State {
        delta: 0x1F87,
        theta: 0x65A5,
        mps_next: 25,
        lps_next: 21,
    }, // 23
    State {
        delta: 0x1F87,
        theta: 0x65A5,
        mps_next: 26,
        lps_next: 22,
    }, // 24
    State {
        delta: 0x1BBB,
        theta: 0x6962,
        mps_next: 27,
        lps_next: 23,
    }, // 25
    State {
        delta: 0x1BBB,
        theta: 0x6962,
        mps_next: 28,
        lps_next: 24,
    }, // 26
    State {
        delta: 0x1845,
        theta: 0x6CA2,
        mps_next: 29,
        lps_next: 25,
    }, // 27
    State {
        delta: 0x1845,
        theta: 0x6CA2,
        mps_next: 30,
        lps_next: 26,
    }, // 28
    State {
        delta: 0x1523,
        theta: 0x6F74,
        mps_next: 31,
        lps_next: 27,
    }, // 29
    State {
        delta: 0x1523,
        theta: 0x6F74,
        mps_next: 32,
        lps_next: 28,
    }, // 30
    State {
        delta: 0x1253,
        theta: 0x71E6,
        mps_next: 33,
        lps_next: 29,
    }, // 31
    State {
        delta: 0x1253,
        theta: 0x71E6,
        mps_next: 34,
        lps_next: 30,
    }, // 32
    State {
        delta: 0x0FCF,
        theta: 0x7404,
        mps_next: 35,
        lps_next: 31,
    }, // 33
    State {
        delta: 0x0FCF,
        theta: 0x7404,
        mps_next: 36,
        lps_next: 32,
    }, // 34
    State {
        delta: 0x0D95,
        theta: 0x75D6,
        mps_next: 37,
        lps_next: 33,
    }, // 35
    State {
        delta: 0x0D95,
        theta: 0x75D6,
        mps_next: 38,
        lps_next: 34,
    }, // 36
    State {
        delta: 0x0B9D,
        theta: 0x7768,
        mps_next: 39,
        lps_next: 35,
    }, // 37
    State {
        delta: 0x0B9D,
        theta: 0x7768,
        mps_next: 40,
        lps_next: 36,
    }, // 38
    State {
        delta: 0x09E3,
        theta: 0x78C2,
        mps_next: 41,
        lps_next: 37,
    }, // 39
    State {
        delta: 0x09E3,
        theta: 0x78C2,
        mps_next: 42,
        lps_next: 38,
    }, // 40
    State {
        delta: 0x0861,
        theta: 0x79EA,
        mps_next: 43,
        lps_next: 39,
    }, // 41
    State {
        delta: 0x0861,
        theta: 0x79EA,
        mps_next: 44,
        lps_next: 40,
    }, // 42
    State {
        delta: 0x0711,
        theta: 0x7AE7,
        mps_next: 45,
        lps_next: 41,
    }, // 43
    State {
        delta: 0x0711,
        theta: 0x7AE7,
        mps_next: 46,
        lps_next: 42,
    }, // 44
    State {
        delta: 0x05F1,
        theta: 0x7BBE,
        mps_next: 47,
        lps_next: 43,
    }, // 45
    State {
        delta: 0x05F1,
        theta: 0x7BBE,
        mps_next: 48,
        lps_next: 44,
    }, // 46
    State {
        delta: 0x04F9,
        theta: 0x7C75,
        mps_next: 49,
        lps_next: 45,
    }, // 47
    State {
        delta: 0x04F9,
        theta: 0x7C75,
        mps_next: 50,
        lps_next: 46,
    }, // 48
    State {
        delta: 0x0425,
        theta: 0x7D0F,
        mps_next: 51,
        lps_next: 47,
    }, // 49
    State {
        delta: 0x0425,
        theta: 0x7D0F,
        mps_next: 52,
        lps_next: 48,
    }, // 50
    State {
        delta: 0x0371,
        theta: 0x7D91,
        mps_next: 53,
        lps_next: 49,
    }, // 51
    State {
        delta: 0x0371,
        theta: 0x7D91,
        mps_next: 54,
        lps_next: 50,
    }, // 52
    State {
        delta: 0x02D9,
        theta: 0x7DFE,
        mps_next: 55,
        lps_next: 51,
    }, // 53
    State {
        delta: 0x02D9,
        theta: 0x7DFE,
        mps_next: 56,
        lps_next: 52,
    }, // 54
    State {
        delta: 0x0259,
        theta: 0x7E5A,
        mps_next: 57,
        lps_next: 53,
    }, // 55
    State {
        delta: 0x0259,
        theta: 0x7E5A,
        mps_next: 58,
        lps_next: 54,
    }, // 56
    State {
        delta: 0x01ED,
        theta: 0x7EA6,
        mps_next: 59,
        lps_next: 55,
    }, // 57
    State {
        delta: 0x01ED,
        theta: 0x7EA6,
        mps_next: 60,
        lps_next: 56,
    }, // 58
    State {
        delta: 0x0193,
        theta: 0x7EE6,
        mps_next: 61,
        lps_next: 57,
    }, // 59
    State {
        delta: 0x0193,
        theta: 0x7EE6,
        mps_next: 62,
        lps_next: 58,
    }, // 60
    State {
        delta: 0x0149,
        theta: 0x7F1A,
        mps_next: 63,
        lps_next: 59,
    }, // 61
    State {
        delta: 0x0149,
        theta: 0x7F1A,
        mps_next: 64,
        lps_next: 60,
    }, // 62
    State {
        delta: 0x010B,
        theta: 0x7F45,
        mps_next: 65,
        lps_next: 61,
    }, // 63
    State {
        delta: 0x010B,
        theta: 0x7F45,
        mps_next: 66,
        lps_next: 62,
    }, // 64
    State {
        delta: 0x00D5,
        theta: 0x7F6B,
        mps_next: 67,
        lps_next: 63,
    }, // 65
    State {
        delta: 0x00D5,
        theta: 0x7F6B,
        mps_next: 68,
        lps_next: 64,
    }, // 66
    State {
        delta: 0x00A5,
        theta: 0x7F8D,
        mps_next: 69,
        lps_next: 65,
    }, // 67
    State {
        delta: 0x00A5,
        theta: 0x7F8D,
        mps_next: 70,
        lps_next: 66,
    }, // 68
    State {
        delta: 0x007B,
        theta: 0x7FAA,
        mps_next: 71,
        lps_next: 67,
    }, // 69
    State {
        delta: 0x007B,
        theta: 0x7FAA,
        mps_next: 72,
        lps_next: 68,
    }, // 70
    State {
        delta: 0x0057,
        theta: 0x7FC3,
        mps_next: 73,
        lps_next: 69,
    }, // 71
    State {
        delta: 0x0057,
        theta: 0x7FC3,
        mps_next: 74,
        lps_next: 70,
    }, // 72
    State {
        delta: 0x003B,
        theta: 0x7FD7,
        mps_next: 75,
        lps_next: 71,
    }, // 73
    State {
        delta: 0x003B,
        theta: 0x7FD7,
        mps_next: 76,
        lps_next: 72,
    }, // 74
    State {
        delta: 0x0023,
        theta: 0x7FE7,
        mps_next: 77,
        lps_next: 73,
    }, // 75
    State {
        delta: 0x0023,
        theta: 0x7FE7,
        mps_next: 78,
        lps_next: 74,
    }, // 76
    State {
        delta: 0x0013,
        theta: 0x7FF2,
        mps_next: 79,
        lps_next: 75,
    }, // 77
    State {
        delta: 0x0013,
        theta: 0x7FF2,
        mps_next: 80,
        lps_next: 76,
    }, // 78
    State {
        delta: 0x0007,
        theta: 0x7FFA,
        mps_next: 81,
        lps_next: 77,
    }, // 79
    State {
        delta: 0x0007,
        theta: 0x7FFA,
        mps_next: 82,
        lps_next: 78,
    }, // 80
    State {
        delta: 0x0001,
        theta: 0x7FFF,
        mps_next: 81,
        lps_next: 79,
    }, // 81
    State {
        delta: 0x0001,
        theta: 0x7FFF,
        mps_next: 82,
        lps_next: 80,
    }, // 82
    State {
        delta: 0x5695,
        theta: 0x0000,
        mps_next: 9,
        lps_next: 85,
    }, // 83
    State {
        delta: 0x24EE,
        theta: 0x0000,
        mps_next: 86,
        lps_next: 226,
    }, // 84
    State {
        delta: 0x8000,
        theta: 0x0000,
        mps_next: 5,
        lps_next: 6,
    }, // 85
    State {
        delta: 0x0D30,
        theta: 0x0000,
        mps_next: 88,
        lps_next: 176,
    }, // 86
    State {
        delta: 0x481A,
        theta: 0x0000,
        mps_next: 89,
        lps_next: 143,
    }, // 87
    State {
        delta: 0x0481,
        theta: 0x0000,
        mps_next: 90,
        lps_next: 138,
    }, // 88
    State {
        delta: 0x3579,
        theta: 0x0000,
        mps_next: 91,
        lps_next: 141,
    }, // 89
    State {
        delta: 0x017A,
        theta: 0x0000,
        mps_next: 92,
        lps_next: 112,
    }, // 90
    State {
        delta: 0x24EF,
        theta: 0x0000,
        mps_next: 93,
        lps_next: 135,
    }, // 91
    State {
        delta: 0x007B,
        theta: 0x0000,
        mps_next: 94,
        lps_next: 104,
    }, // 92
    State {
        delta: 0x1978,
        theta: 0x0000,
        mps_next: 95,
        lps_next: 133,
    }, // 93
    State {
        delta: 0x0028,
        theta: 0x0000,
        mps_next: 96,
        lps_next: 100,
    }, // 94
    State {
        delta: 0x10CA,
        theta: 0x0000,
        mps_next: 97,
        lps_next: 129,
    }, // 95
    State {
        delta: 0x000D,
        theta: 0x0000,
        mps_next: 82,
        lps_next: 98,
    }, // 96
    State {
        delta: 0x0B5D,
        theta: 0x0000,
        mps_next: 99,
        lps_next: 127,
    }, // 97
    State {
        delta: 0x0034,
        theta: 0x0000,
        mps_next: 76,
        lps_next: 72,
    }, // 98
    State {
        delta: 0x078A,
        theta: 0x0000,
        mps_next: 101,
        lps_next: 125,
    }, // 99
    State {
        delta: 0x00A0,
        theta: 0x0000,
        mps_next: 70,
        lps_next: 102,
    }, // 100
    State {
        delta: 0x050F,
        theta: 0x0000,
        mps_next: 103,
        lps_next: 123,
    }, // 101
    State {
        delta: 0x0117,
        theta: 0x0000,
        mps_next: 66,
        lps_next: 60,
    }, // 102
    State {
        delta: 0x0358,
        theta: 0x0000,
        mps_next: 105,
        lps_next: 121,
    }, // 103
    State {
        delta: 0x01EA,
        theta: 0x0000,
        mps_next: 106,
        lps_next: 110,
    }, // 104
    State {
        delta: 0x0234,
        theta: 0x0000,
        mps_next: 107,
        lps_next: 119,
    }, // 105
    State {
        delta: 0x0144,
        theta: 0x0000,
        mps_next: 66,
        lps_next: 108,
    }, // 106
    State {
        delta: 0x0173,
        theta: 0x0000,
        mps_next: 109,
        lps_next: 117,
    }, // 107
    State {
        delta: 0x0234,
        theta: 0x0000,
        mps_next: 60,
        lps_next: 54,
    }, // 108
    State {
        delta: 0x00F5,
        theta: 0x0000,
        mps_next: 111,
        lps_next: 115,
    }, // 109
    State {
        delta: 0x0353,
        theta: 0x0000,
        mps_next: 56,
        lps_next: 48,
    }, // 110
    State {
        delta: 0x00A1,
        theta: 0x0000,
        mps_next: 69,
        lps_next: 113,
    }, // 111
    State {
        delta: 0x05C5,
        theta: 0x0000,
        mps_next: 114,
        lps_next: 134,
    }, // 112
    State {
        delta: 0x011A,
        theta: 0x0000,
        mps_next: 65,
        lps_next: 59,
    }, // 113
    State {
        delta: 0x03CF,
        theta: 0x0000,
        mps_next: 116,
        lps_next: 132,
    }, // 114
    State {
        delta: 0x01AA,
        theta: 0x0000,
        mps_next: 61,
        lps_next: 55,
    }, // 115
    State {
        delta: 0x0285,
        theta: 0x0000,
        mps_next: 118,
        lps_next: 130,
    }, // 116
    State {
        delta: 0x0286,
        theta: 0x0000,
        mps_next: 57,
        lps_next: 51,
    }, // 117
    State {
        delta: 0x01AB,
        theta: 0x0000,
        mps_next: 120,
        lps_next: 128,
    }, // 118
    State {
        delta: 0x03D3,
        theta: 0x0000,
        mps_next: 53,
        lps_next: 47,
    }, // 119
    State {
        delta: 0x011A,
        theta: 0x0000,
        mps_next: 122,
        lps_next: 126,
    }, // 120
    State {
        delta: 0x05C5,
        theta: 0x0000,
        mps_next: 49,
        lps_next: 41,
    }, // 121
    State {
        delta: 0x00BA,
        theta: 0x0000,
        mps_next: 124,
        lps_next: 62,
    }, // 122
    State {
        delta: 0x08AD,
        theta: 0x0000,
        mps_next: 43,
        lps_next: 37,
    }, // 123
    State {
        delta: 0x007A,
        theta: 0x0000,
        mps_next: 72,
        lps_next: 66,
    }, // 124
    State {
        delta: 0x0CCC,
        theta: 0x0000,
        mps_next: 39,
        lps_next: 31,
    }, // 125
    State {
        delta: 0x01EB,
        theta: 0x0000,
        mps_next: 60,
        lps_next: 54,
    }, // 126
    State {
        delta: 0x1302,
        theta: 0x0000,
        mps_next: 33,
        lps_next: 25,
    }, // 127
    State {
        delta: 0x02E6,
        theta: 0x0000,
        mps_next: 56,
        lps_next: 50,
    }, // 128
    State {
        delta: 0x1B81,
        theta: 0x0000,
        mps_next: 29,
        lps_next: 131,
    }, // 129
    State {
        delta: 0x045E,
        theta: 0x0000,
        mps_next: 52,
        lps_next: 46,
    }, // 130
    State {
        delta: 0x24EF,
        theta: 0x0000,
        mps_next: 23,
        lps_next: 17,
    }, // 131
    State {
        delta: 0x0690,
        theta: 0x0000,
        mps_next: 48,
        lps_next: 40,
    }, // 132
    State {
        delta: 0x2865,
        theta: 0x0000,
        mps_next: 23,
        lps_next: 15,
    }, // 133
    State {
        delta: 0x09DE,
        theta: 0x0000,
        mps_next: 42,
        lps_next: 136,
    }, // 134
    State {
        delta: 0x3987,
        theta: 0x0000,
        mps_next: 137,
        lps_next: 7,
    }, // 135
    State {
        delta: 0x0DC8,
        theta: 0x0000,
        mps_next: 38,
        lps_next: 32,
    }, // 136
    State {
        delta: 0x2C99,
        theta: 0x0000,
        mps_next: 21,
        lps_next: 139,
    }, // 137
    State {
        delta: 0x10CA,
        theta: 0x0000,
        mps_next: 140,
        lps_next: 172,
    }, // 138
    State {
        delta: 0x3B5F,
        theta: 0x0000,
        mps_next: 15,
        lps_next: 9,
    }, // 139
    State {
        delta: 0x0B5D,
        theta: 0x0000,
        mps_next: 142,
        lps_next: 170,
    }, // 140
    State {
        delta: 0x5695,
        theta: 0x0000,
        mps_next: 9,
        lps_next: 85,
    }, // 141
    State {
        delta: 0x078A,
        theta: 0x0000,
        mps_next: 144,
        lps_next: 168,
    }, // 142
    State {
        delta: 0x8000,
        theta: 0x0000,
        mps_next: 141,
        lps_next: 248,
    }, // 143
    State {
        delta: 0x050F,
        theta: 0x0000,
        mps_next: 146,
        lps_next: 166,
    }, // 144
    State {
        delta: 0x24EE,
        theta: 0x0000,
        mps_next: 147,
        lps_next: 247,
    }, // 145
    State {
        delta: 0x0358,
        theta: 0x0000,
        mps_next: 148,
        lps_next: 164,
    }, // 146
    State {
        delta: 0x0D30,
        theta: 0x0000,
        mps_next: 149,
        lps_next: 197,
    }, // 147
    State {
        delta: 0x0234,
        theta: 0x0000,
        mps_next: 150,
        lps_next: 162,
    }, // 148
    State {
        delta: 0x0481,
        theta: 0x0000,
        mps_next: 151,
        lps_next: 95,
    }, // 149
    State {
        delta: 0x0173,
        theta: 0x0000,
        mps_next: 152,
        lps_next: 160,
    }, // 150
    State {
        delta: 0x017A,
        theta: 0x0000,
        mps_next: 153,
        lps_next: 173,
    }, // 151
    State {
        delta: 0x00F5,
        theta: 0x0000,
        mps_next: 154,
        lps_next: 158,
    }, // 152
    State {
        delta: 0x007B,
        theta: 0x0000,
        mps_next: 155,
        lps_next: 165,
    }, // 153
    State {
        delta: 0x00A1,
        theta: 0x0000,
        mps_next: 70,
        lps_next: 156,
    }, // 154
    State {
        delta: 0x0028,
        theta: 0x0000,
        mps_next: 157,
        lps_next: 161,
    }, // 155
    State {
        delta: 0x011A,
        theta: 0x0000,
        mps_next: 66,
        lps_next: 60,
    }, // 156
    State {
        delta: 0x000D,
        theta: 0x0000,
        mps_next: 81,
        lps_next: 159,
    }, // 157
    State {
        delta: 0x01AA,
        theta: 0x0000,
        mps_next: 62,
        lps_next: 56,
    }, // 158
    State {
        delta: 0x0034,
        theta: 0x0000,
        mps_next: 75,
        lps_next: 71,
    }, // 159
    State {
        delta: 0x0286,
        theta: 0x0000,
        mps_next: 58,
        lps_next: 52,
    }, // 160
    State {
        delta: 0x00A0,
        theta: 0x0000,
        mps_next: 69,
        lps_next: 163,
    }, // 161
    State {
        delta: 0x03D3,
        theta: 0x0000,
        mps_next: 54,
        lps_next: 48,
    }, // 162
    State {
        delta: 0x0117,
        theta: 0x0000,
        mps_next: 65,
        lps_next: 59,
    }, // 163
    State {
        delta: 0x05C5,
        theta: 0x0000,
        mps_next: 50,
        lps_next: 42,
    }, // 164
    State {
        delta: 0x01EA,
        theta: 0x0000,
        mps_next: 167,
        lps_next: 171,
    }, // 165
    State {
        delta: 0x08AD,
        theta: 0x0000,
        mps_next: 44,
        lps_next: 38,
    }, // 166
    State {
        delta: 0x0144,
        theta: 0x0000,
        mps_next: 65,
        lps_next: 169,
    }, // 167
    State {
        delta: 0x0CCC,
        theta: 0x0000,
        mps_next: 40,
        lps_next: 32,
    }, // 168
    State {
        delta: 0x0234,
        theta: 0x0000,
        mps_next: 59,
        lps_next: 53,
    }, // 169
    State {
        delta: 0x1302,
        theta: 0x0000,
        mps_next: 34,
        lps_next: 26,
    }, // 170
    State {
        delta: 0x0353,
        theta: 0x0000,
        mps_next: 55,
        lps_next: 47,
    }, // 171
    State {
        delta: 0x1B81,
        theta: 0x0000,
        mps_next: 30,
        lps_next: 174,
    }, // 172
    State {
        delta: 0x05C5,
        theta: 0x0000,
        mps_next: 175,
        lps_next: 193,
    }, // 173
    State {
        delta: 0x24EF,
        theta: 0x0000,
        mps_next: 24,
        lps_next: 18,
    }, // 174
    State {
        delta: 0x03CF,
        theta: 0x0000,
        mps_next: 177,
        lps_next: 191,
    }, // 175
    State {
        delta: 0x2B74,
        theta: 0x0000,
        mps_next: 178,
        lps_next: 222,
    }, // 176
    State {
        delta: 0x0285,
        theta: 0x0000,
        mps_next: 179,
        lps_next: 189,
    }, // 177
    State {
        delta: 0x201D,
        theta: 0x0000,
        mps_next: 180,
        lps_next: 218,
    }, // 178
    State {
        delta: 0x01AB,
        theta: 0x0000,
        mps_next: 181,
        lps_next: 187,
    }, // 179
    State {
        delta: 0x1715,
        theta: 0x0000,
        mps_next: 182,
        lps_next: 216,
    }, // 180
    State {
        delta: 0x011A,
        theta: 0x0000,
        mps_next: 183,
        lps_next: 185,
    }, // 181
    State {
        delta: 0x0FB7,
        theta: 0x0000,
        mps_next: 184,
        lps_next: 214,
    }, // 182
    State {
        delta: 0x00BA,
        theta: 0x0000,
        mps_next: 69,
        lps_next: 61,
    }, // 183
    State {
        delta: 0x0A67,
        theta: 0x0000,
        mps_next: 186,
        lps_next: 212,
    }, // 184
    State {
        delta: 0x01EB,
        theta: 0x0000,
        mps_next: 59,
        lps_next: 53,
    }, // 185
    State {
        delta: 0x06E7,
        theta: 0x0000,
        mps_next: 188,
        lps_next: 210,
    }, // 186
    State {
        delta: 0x02E6,
        theta: 0x0000,
        mps_next: 55,
        lps_next: 49,
    }, // 187
    State {
        delta: 0x0496,
        theta: 0x0000,
        mps_next: 190,
        lps_next: 208,
    }, // 188
    State {
        delta: 0x045E,
        theta: 0x0000,
        mps_next: 51,
        lps_next: 45,
    }, // 189
    State {
        delta: 0x030D,
        theta: 0x0000,
        mps_next: 192,
        lps_next: 206,
    }, // 190
    State {
        delta: 0x0690,
        theta: 0x0000,
        mps_next: 47,
        lps_next: 39,
    }, // 191
    State {
        delta: 0x0206,
        theta: 0x0000,
        mps_next: 194,
        lps_next: 204,
    }, // 192
    State {
        delta: 0x09DE,
        theta: 0x0000,
        mps_next: 41,
        lps_next: 195,
    }, // 193
    State {
        delta: 0x0155,
        theta: 0x0000,
        mps_next: 196,
        lps_next: 202,
    }, // 194
    State {
        delta: 0x0DC8,
        theta: 0x0000,
        mps_next: 37,
        lps_next: 31,
    }, // 195
    State {
        delta: 0x00E1,
        theta: 0x0000,
        mps_next: 198,
        lps_next: 200,
    }, // 196
    State {
        delta: 0x2B74,
        theta: 0x0000,
        mps_next: 199,
        lps_next: 243,
    }, // 197
    State {
        delta: 0x0094,
        theta: 0x0000,
        mps_next: 72,
        lps_next: 64,
    }, // 198
    State {
        delta: 0x201D,
        theta: 0x0000,
        mps_next: 201,
        lps_next: 239,
    }, // 199
    State {
        delta: 0x0188,
        theta: 0x0000,
        mps_next: 62,
        lps_next: 56,
    }, // 200
    State {
        delta: 0x1715,
        theta: 0x0000,
        mps_next: 203,
        lps_next: 237,
    }, // 201
    State {
        delta: 0x0252,
        theta: 0x0000,
        mps_next: 58,
        lps_next: 52,
    }, // 202
    State {
        delta: 0x0FB7,
        theta: 0x0000,
        mps_next: 205,
        lps_next: 235,
    }, // 203
    State {
        delta: 0x0383,
        theta: 0x0000,
        mps_next: 54,
        lps_next: 48,
    }, // 204
    State {
        delta: 0x0A67,
        theta: 0x0000,
        mps_next: 207,
        lps_next: 233,
    }, // 205
    State {
        delta: 0x0547,
        theta: 0x0000,
        mps_next: 50,
        lps_next: 44,
    }, // 206
    State {
        delta: 0x06E7,
        theta: 0x0000,
        mps_next: 209,
        lps_next: 231,
    }, // 207
    State {
        delta: 0x07E2,
        theta: 0x0000,
        mps_next: 46,
        lps_next: 38,
    }, // 208
    State {
        delta: 0x0496,
        theta: 0x0000,
        mps_next: 211,
        lps_next: 229,
    }, // 209
    State {
        delta: 0x0BC0,
        theta: 0x0000,
        mps_next: 40,
        lps_next: 34,
    }, // 210
    State {
        delta: 0x030D,
        theta: 0x0000,
        mps_next: 213,
        lps_next: 227,
    }, // 211
    State {
        delta: 0x1178,
        theta: 0x0000,
        mps_next: 36,
        lps_next: 28,
    }, // 212
    State {
        delta: 0x0206,
        theta: 0x0000,
        mps_next: 215,
        lps_next: 225,
    }, // 213
    State {
        delta: 0x19DA,
        theta: 0x0000,
        mps_next: 30,
        lps_next: 22,
    }, // 214
    State {
        delta: 0x0155,
        theta: 0x0000,
        mps_next: 217,
        lps_next: 223,
    }, // 215
    State {
        delta: 0x24EF,
        theta: 0x0000,
        mps_next: 26,
        lps_next: 16,
    }, // 216
    State {
        delta: 0x00E1,
        theta: 0x0000,
        mps_next: 219,
        lps_next: 221,
    }, // 217
    State {
        delta: 0x320E,
        theta: 0x0000,
        mps_next: 20,
        lps_next: 220,
    }, // 218
    State {
        delta: 0x0094,
        theta: 0x0000,
        mps_next: 71,
        lps_next: 63,
    }, // 219
    State {
        delta: 0x432A,
        theta: 0x0000,
        mps_next: 14,
        lps_next: 8,
    }, // 220
    State {
        delta: 0x0188,
        theta: 0x0000,
        mps_next: 61,
        lps_next: 55,
    }, // 221
    State {
        delta: 0x447D,
        theta: 0x0000,
        mps_next: 14,
        lps_next: 224,
    }, // 222
    State {
        delta: 0x0252,
        theta: 0x0000,
        mps_next: 57,
        lps_next: 51,
    }, // 223
    State {
        delta: 0x5ECE,
        theta: 0x0000,
        mps_next: 8,
        lps_next: 2,
    }, // 224
    State {
        delta: 0x0383,
        theta: 0x0000,
        mps_next: 53,
        lps_next: 47,
    }, // 225
    State {
        delta: 0x8000,
        theta: 0x0000,
        mps_next: 228,
        lps_next: 87,
    }, // 226
    State {
        delta: 0x0547,
        theta: 0x0000,
        mps_next: 49,
        lps_next: 43,
    }, // 227
    State {
        delta: 0x481A,
        theta: 0x0000,
        mps_next: 230,
        lps_next: 246,
    }, // 228
    State {
        delta: 0x07E2,
        theta: 0x0000,
        mps_next: 45,
        lps_next: 37,
    }, // 229
    State {
        delta: 0x3579,
        theta: 0x0000,
        mps_next: 232,
        lps_next: 244,
    }, // 230
    State {
        delta: 0x0BC0,
        theta: 0x0000,
        mps_next: 39,
        lps_next: 33,
    }, // 231
    State {
        delta: 0x24EF,
        theta: 0x0000,
        mps_next: 234,
        lps_next: 238,
    }, // 232
    State {
        delta: 0x1178,
        theta: 0x0000,
        mps_next: 35,
        lps_next: 27,
    }, // 233
    State {
        delta: 0x1978,
        theta: 0x0000,
        mps_next: 138,
        lps_next: 236,
    }, // 234
    State {
        delta: 0x19DA,
        theta: 0x0000,
        mps_next: 29,
        lps_next: 21,
    }, // 235
    State {
        delta: 0x2865,
        theta: 0x0000,
        mps_next: 24,
        lps_next: 16,
    }, // 236
    State {
        delta: 0x24EF,
        theta: 0x0000,
        mps_next: 25,
        lps_next: 15,
    }, // 237
    State {
        delta: 0x3987,
        theta: 0x0000,
        mps_next: 240,
        lps_next: 8,
    }, // 238
    State {
        delta: 0x320E,
        theta: 0x0000,
        mps_next: 19,
        lps_next: 241,
    }, // 239
    State {
        delta: 0x2C99,
        theta: 0x0000,
        mps_next: 22,
        lps_next: 242,
    }, // 240
    State {
        delta: 0x432A,
        theta: 0x0000,
        mps_next: 13,
        lps_next: 7,
    }, // 241
    State {
        delta: 0x3B5F,
        theta: 0x0000,
        mps_next: 16,
        lps_next: 10,
    }, // 242
    State {
        delta: 0x447D,
        theta: 0x0000,
        mps_next: 13,
        lps_next: 245,
    }, // 243
    State {
        delta: 0x5695,
        theta: 0x0000,
        mps_next: 10,
        lps_next: 2,
    }, // 244
    State {
        delta: 0x5ECE,
        theta: 0x0000,
        mps_next: 7,
        lps_next: 1,
    }, // 245
    State {
        delta: 0x8000,
        theta: 0x0000,
        mps_next: 244,
        lps_next: 83,
    }, // 246
    State {
        delta: 0x8000,
        theta: 0x0000,
        mps_next: 249,
        lps_next: 250,
    }, // 247
    State {
        delta: 0x5695,
        theta: 0x0000,
        mps_next: 10,
        lps_next: 2,
    }, // 248
    State {
        delta: 0x481A,
        theta: 0x0000,
        mps_next: 89,
        lps_next: 143,
    }, // 249
    State {
        delta: 0x481A,
        theta: 0x0000,
        mps_next: 230,
        lps_next: 246,
    }, // 250
];
