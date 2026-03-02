# Kuantum Kriptografi -- Kuantum Sonrası Tehdit Analizi

Bu bölüm, kuantum bilgisayarların GroveDB'de kullanılan kriptografik temel bileşenleri ve bunun üzerine inşa edilen korumalı işlem protokollerini (Orchard, Dash Platform) nasıl etkileyeceğini analiz eder. Hangi bileşenlerin savunmasız, hangilerinin güvenli olduğunu, "şimdi topla, sonra çöz" kavramının depolanan veriler için ne anlama geldiğini ve hibrit KEM tasarımları dahil hangi azaltma stratejilerinin mevcut olduğunu kapsar.

## Önemli İki Kuantum Algoritması

Pratikte kriptografi ile ilgili yalnızca iki kuantum algoritması vardır:

**Shor algoritması** ayrık logaritma problemini (ve tamsayı çarpanlarını ayırmayı) polinom zamanda çözer. Pallas gibi 255 bitlik bir eliptik eğri için yaklaşık 510 mantıksal kübit gerektirir -- ancak hata düzeltme yükü ile gerçek gereksinim yaklaşık 4 milyon fiziksel kübittir. Shor algoritması, anahtar boyutundan bağımsız olarak tüm eliptik eğri kriptografisini **tamamen kırar**.

**Grover algoritması** kaba kuvvet araması için ikinci dereceden hızlanma sağlar. 256 bitlik bir simetrik anahtar etkin olarak 128 bit olur. Ancak, 128 bitlik bir anahtar uzayı için Grover'ın devre derinliği hâlâ 2^64 kuantum işlemidir -- birçok kriptograf, dekoherans sınırları nedeniyle bunun gerçek donanım üzerinde asla pratik olmayacağına inanmaktadır. Grover güvenlik marjlarını azaltır ancak iyi parametrelenmiş simetrik kriptografiyi kırmaz.

| Algoritma | Hedefler | Hızlanma | Pratik etki |
|-----------|---------|---------|------------------|
| **Shor** | ECC ayrık logaritma, RSA çarpanlarına ayırma | Üstel (polinom zaman) | ECC'nin **tamamen kırılması** |
| **Grover** | Simetrik anahtar arama, hash ön-görüntü | İkinci dereceden (anahtar bitlerini yarıya indirir) | 256 bit -> 128 bit (hâlâ güvenli) |

## GroveDB'nin Kriptografik Temel Bileşenleri

GroveDB ve Orchard tabanlı korumalı protokol, eliptik eğri ile simetrik/hash tabanlı temel bileşenlerin bir karışımını kullanır. Aşağıdaki tablo, her bir temel bileşeni kuantum savunmasızlığına göre sınıflandırır:

### Kuantuma Karşı Savunmasız (Shor algoritması -- kuantum sonrası 0 bit)

| Temel bileşen | Kullanım yeri | Ne kırılır |
|-----------|-----------|-------------|
| **Pallas ECDLP** | Not taahhütleri (cmx), geçici anahtarlar (epk/esk), görüntüleme anahtarları (ivk), ödeme anahtarları (pk_d), nullifier türetimi | Herhangi bir özel anahtarı kamusal karşılığından kurtar |
| **ECDH anahtar anlaşma** (Pallas) | Not şifreli metinleri için simetrik şifreleme anahtarı türetimi | Paylaşılan sırrı kurtar -> tüm notları çöz |
| **Sinsemilla hash** | CommitmentTree Merkle yolları, devre içi hashleme | Çarpışma direnci ECDLP'ye bağlıdır; Pallas kırıldığında zayıflar |
| **Halo 2 IPA** | ZK ispat sistemi (Pasta eğrileri üzerinde polinom taahhüt) | Yanlış ifadeler için ispat sahteciliği (sahtecilik, yetkisiz harcama) |
| **Pedersen taahhütleri** | İşlem tutarlarını gizleyen değer taahhütleri (cv_net) | Gizli tutarları kurtar; bakiye ispatlarını sahtecilikle oluştur |

### Kuantuma Karşı Güvenli (Grover algoritması -- kuantum sonrası 128+ bit)

| Temel bileşen | Kullanım yeri | Kuantum sonrası güvenlik |
|-----------|-----------|----------------------|
| **Blake3** | Merk ağacı düğüm hashleri, MMR düğümleri, BulkAppendTree durum kökleri, alt ağaç yol önekleri | 128 bit ön-görüntü, 128 bit ikinci ön-görüntü |
| **BLAKE2b-256** | Simetrik anahtar türetimi için KDF, giden şifre anahtarı, PRF^expand | 128 bit ön-görüntü |
| **ChaCha20-Poly1305** | enc_ciphertext ve out_ciphertext şifreleme (256 bit anahtarlar) | 128 bit anahtar arama (güvenli, ancak ECDH üzerinden anahtar türetim yolu güvenli değil) |
| **PRF^expand** (BLAKE2b-512) | rseed'den esk, rcm, psi türetimi | 128 bit PRF güvenliği |

### GroveDB Altyapısı: Tamamen Kuantuma Karşı Güvenli

GroveDB'nin kendi veri yapıları yalnızca Blake3 hashlemesine dayanır:

- **Merk AVL ağaçları** -- düğüm hashleri, combined_value_hash, çocuk hash yayılımı
- **MMR ağaçları** -- iç düğüm hashleri, tepe hesabı, kök türetimi
- **BulkAppendTree** -- tampon hash zincirleri, yoğun Merkle kökleri, dönem MMR
- **CommitmentTree durum kökü** -- `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Alt ağaç yol önekleri** -- yol segmentlerinin Blake3 hashlemesi
- **V1 ispatlar** -- Merk hiyerarşisi üzerinden doğrulama zincirleri

**Değişiklik gerekmez.** GroveDB'nin Merk ağacı ispatları, MMR tutarlılık kontrolleri, BulkAppendTree dönem kökleri ve tüm V1 ispat doğrulama zincirleri kuantum bilgisayarlara karşı güvenli kalır. Hash tabanlı altyapı, sistemin kuantum sonrası en güçlü parçasıdır.

## Geriye Dönük ve Gerçek Zamanlı Tehditler

Bu ayrım, neyin ne zaman düzeltilmesi gerektiğini önceliklendirmek için kritiktir.

**Geriye dönük tehditler** halihazırda depolanan verileri tehlikeye atar. Bir saldırgan bugün verileri kaydeder ve kuantum bilgisayarlar kullanılabilir hale geldiğinde çözer. Bu tehditler **sonradan azaltılamaz** -- veri zincir üzerinde olduğunda yeniden şifrelenemez veya geri alınamaz.

**Gerçek zamanlı tehditler** yalnızca gelecekte oluşturulan işlemleri etkiler. Kuantum bilgisayara sahip bir saldırgan imza veya ispat sahteciliği yapabilir, ancak yalnızca yeni işlemler için. Eski işlemler ağ tarafından zaten doğrulanmış ve onaylanmıştır.

| Tehdit | Tür | Açığa çıkan | Aciliyet |
|--------|------|---------------|---------|
| **Not çözme** (enc_ciphertext) | **Geriye dönük** | Not içerikleri: alıcı, tutar, not, rseed | **Yüksek** -- kalıcı depolama |
| **Değer taahhüt açma** (cv_net) | **Geriye dönük** | İşlem tutarları (ancak gönderen/alıcı değil) | **Orta** -- yalnızca tutarlar |
| **Gönderen kurtarma verileri** (out_ciphertext) | **Geriye dönük** | Göndereninağönderilen notlar için kurtarma anahtarları | **Yüksek** -- kalıcı depolama |
| Harcama yetkilendirme sahteciliği | Gerçek zamanlı | Yeni harcama imzaları sahteleyebilir | Düşük -- QC gelmeden önce yükselt |
| Halo 2 ispat sahteciliği | Gerçek zamanlı | Yeni ispatlar sahteleyebilir (sahtecilik) | Düşük -- QC gelmeden önce yükselt |
| Sinsemilla çarpışması | Gerçek zamanlı | Yeni Merkle yolları sahteleyebilir | Düşük -- ispat sahteciliği kapsamında |
| Bağlama imza sahteciliği | Gerçek zamanlı | Yeni bakiye ispatları sahteleyebilir | Düşük -- QC gelmeden önce yükselt |

### Tam Olarak Ne Açığa Çıkar?

**Not şifreleme kırılırsa** (birincil HNDL tehdidi):

Kuantum saldırgan, Shor algoritması aracılığıyla depolanan `epk`'dan `esk`'yi kurtarır, ECDH paylaşılan sırrı hesaplar, simetrik anahtarı türeterek `enc_ciphertext`'i çözer. Bu, tam not düz metnini ortaya çıkarır:

| Alan | Boyut | Ne ortaya çıkarır |
|-------|------|----------------|
| version | 1 byte | Protokol sürümü (hassas değil) |
| diversifier | 11 bytes | Alıcının adres bileşeni |
| value | 8 bytes | Kesin işlem tutarı |
| rseed | 32 bytes | Nullifier bağlantısını etkinleştirir (işlem grafı anonimliğini kaldırır) |
| memo | 36 bytes (DashMemo) | Uygulama verileri, potansiyel olarak tanımlayıcı |

`rseed` ve `rho` (şifre metni ile birlikte depolanan) ile saldırgan `esk = PRF(rseed, rho)` hesaplayabilir ve geçici anahtar bağlamasını doğrulayabilir. Diversifier ile birleştirildiğinde, tüm işlem geçmişi boyunca girişleri çıktılara bağlar -- **korumalı havuzun tam anonimlik kaybı**.

**Yalnızca değer taahhütleri kırılırsa** (ikincil HNDL tehdidi):

Saldırgan, ECDLP'yi çözerek `cv_net = [v]*V + [rcv]*R`'den `v`'yi kurtarır. Bu, **işlem tutarlarını ortaya çıkarır ancak gönderen veya alıcı kimliklerini değil**. Saldırgan "birisi birisine 5.0 Dash gönderdi" görür ancak not şifrelemeyi de kırmadan tutarı herhangi bir adrese veya kimliğe bağlayamaz.

Tek başına, bağlantı olmadan tutarlar sınırlı kullanıma sahiptir. Ancak dış verilerle (zamanlama, bilinen faturalar, kamusal taleplerle eşleşen tutarlar) birleştirildiğinde korelasyon saldırıları mümkün hale gelir.

## "Şimdi Topla, Sonra Çöz" Saldırısı

Bu, en acil ve pratik kuantum tehdididir.

**Saldırı modeli:** Devlet düzeyinde bir saldırgan (veya yeterli depolama alanı olan herhangi bir taraf) bugün zincir üzerindeki tüm korumalı işlem verilerini kaydeder. Bu veriler blokzincirde herkese açık ve değiştirilemezdir. Saldırgan kriptografik olarak ilgili bir kuantum bilgisayar (CRQC) bekler, ardından:

```text
Step 1: Read stored record from CommitmentTree BulkAppendTree:
        cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Step 2: Solve ECDLP on Pallas via Shor's algorithm:
        epk = [esk] * g_d  →  recover esk

Step 3: Compute shared secret:
        shared_secret = [esk] * pk_d

Step 4: Derive symmetric key (BLAKE2b is quantum-safe, but input is compromised):
        K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Step 5: Decrypt enc_ciphertext using ChaCha20-Poly1305:
        → version || diversifier || value || rseed || memo

Step 6: With rseed + rho, link nullifiers to note commitments:
        esk = PRF(rseed, rho)
        → full transaction graph reconstruction
```

**Temel içerik:** Simetrik şifreleme (ChaCha20-Poly1305) tamamen kuantuma karşı güvenlidir. Savunmasızlık tamamen **anahtar türetim yolunda** -- simetrik anahtar ECDH paylaşılan sırrından türetilir ve ECDH Shor algoritması tarafından kırılır. Saldırgan şifrelemeyi kırmaz; anahtarı kurtarır.

**Geriye dönüklük:** Bu saldırı **tamamen geriye dönüktür**. Bir CRQC mevcut olduğunda zincir üzerinde depolanan her şifrelenmiş not çözülebilir. Veriler sonradan yeniden şifrelenemez veya korunamaz. Bu nedenle veriler depolanmadan önce, sonra değil, ele alınmalıdır.

## Azaltma: Hibrit KEM (ML-KEM + ECDH)

HNDL'ye karşı savunma, simetrik şifreleme anahtarını **iki bağımsız anahtar anlaşma mekanizmasından** türetmektir, böylece yalnızca birini kırmak yetersiz kalır. Buna hibrit KEM denir.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM, Modül Hatalı Öğrenme (MLWE) problemine dayanan NIST standartlaştırılmış (FIPS 203, Ağustos 2024) kuantum sonrası anahtar kapsülleme mekanizmasıdır.

| Parametre | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Açık anahtar (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Şifre metni (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Paylaşılan sır | 32 bytes | 32 bytes | 32 bytes |
| NIST Kategorisi | 1 (128 bit) | **3 (192 bit)** | 5 (256 bit) |

**ML-KEM-768** önerilen seçimdir -- X-Wing, Signal'in PQXDH'si ve Chrome/Firefox TLS hibrit anahtar değişiminde kullanılan parametre setidir. Kategori 3, gelecekteki kafes kriptanaliz gelişmelerine karşı rahat bir marj sağlar.

### Hibrit Şemanın Çalışması

**Mevcut akış (savunmasız):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Hibrit akış (kuantuma dirençli):**

```text
Sender:
  esk = PRF(rseed, rho)                    // unchanged
  epk = [esk] * g_d                         // unchanged
  ss_ecdh = [esk] * pk_d                    // same ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NEW: lattice-based KEM
                                                // ek_pq from recipient's address

  K_enc = BLAKE2b(                          // MODIFIED: combines both secrets
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // unchanged
```

**Alıcı şifre çözme:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Güvenlik Garantisi

Birleşik KEM, **herhangi bir** bileşen KEM güvenliyse IND-CCA2 güvenlidir. Bu, PRF (BLAKE2b uygun) kullanan KEM birleştiricileri için [Giacon, Heuer ve Poettering (2018)](https://eprint.iacr.org/2018/024) tarafından resmi olarak kanıtlanmıştır ve [X-Wing güvenlik kanıtı](https://eprint.iacr.org/2024/039) tarafından bağımsız olarak kanıtlanmıştır.

| Senaryo | ECDH | ML-KEM | Birleşik anahtar | Durum |
|----------|------|--------|-------------|--------|
| Klasik dünya | Güvenli | Güvenli | **Güvenli** | İkisi de sağlam |
| Kuantum ECC'yi kırar | **Kırılmış** | Güvenli | **Güvenli** | ML-KEM korur |
| Kafes gelişmeleri ML-KEM'i kırar | Güvenli | **Kırılmış** | **Güvenli** | ECDH korur (bugünkü gibi) |
| İkisi de kırılmış | Kırılmış | Kırılmış | **Kırılmış** | İki eş zamanlı atılım gerektirir |

### Boyut Etkisi

Hibrit KEM, depolanan her nota ML-KEM-768 şifre metnini (1,088 bytes) ekler ve göndereninağkurtarma için ML-KEM paylaşılan sırrı içermesi amacıyla giden şifre metnini genişletir:

**Not başına depolanan kayıt:**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Current (280 bytes)         Hybrid (1,400 bytes)               │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NEW       │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**Ölçekte depolama:**

| Not sayısı | Mevcut (280 B) | Hibrit (1,400 B) | Fark |
|-------|----------------|------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Adres boyutu:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

1,184 byte ML-KEM açık anahtarı, gönderenlerin kapsülleme yapabilmesi için adrese dahil edilmelidir. Yaklaşık 1,960 Bech32m karakter ile büyüktür ancak bir QR koduna (maksimum ~2,953 alfanümerik karakter) hâlâ sığar.

### Anahtar Yönetimi

ML-KEM anahtar çifti, harcama anahtarından deterministik olarak türetilir:

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (all existing Orchard key derivation unchanged)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (public, included in address)
              dk_pq: 2,400 bytes (private, part of viewing key)
```

**Yedekleme değişikliği gerekmez.** Mevcut 24 kelimelik tohum cümlesi ML-KEM anahtarını kapsar çünkü harcama anahtarından deterministik olarak türetilir. Cüzdan kurtarma eskisi gibi çalışır.

**Çeşitlendirilmiş adresler** hepsi aynı `ek_pq`'yu paylaşıyor çünkü ML-KEM'de Pallas skaler çarpımı gibi doğal bir çeşitlendirme mekanizması yok. Bu, bir kullanıcının iki adresine sahip bir gözlemcinin `ek_pq`'yu karşılaştırarak bunları bağlayabileceği anlamına gelir.

### Deneme Şifre Çözme Performansı

| Adım | Mevcut | Hibrit | Fark |
|------|---------|--------|-------|
| Pallas ECDH | ~100 us | ~100 us | -- |
| ML-KEM-768 Decaps | -- | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | -- |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | -- |
| **Not başına toplam** | **~101 us** | **~141 us** | **+%40 ek yük** |

100,000 not tarama: ~10.1 sn -> ~14.1 sn. Ek yük anlamlıdır ancak engelleyici değildir. ML-KEM kapsülden çıkarma, (eliptik eğri işlemlerinin aksine) yığınlama avantajı olmadan sabit zamanlıdır, bu nedenle doğrusal olarak ölçeklenir.

### ZK Devreleri Üzerindeki Etkisi

**Yok.** Hibrit KEM tamamen taşıma/şifreleme katmanındadır. Halo 2 devresi not varlığını, nullifier doğruluğunu ve değer dengesini kanıtlar -- şifreleme hakkında hiçbir şey kanıtlamaz. İspat anahtarları, doğrulama anahtarları veya devre kısıtlamalarında değişiklik yoktur.

### Sektörle Karşılaştırma

| Sistem | Yaklaşım | Durum |
|--------|----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, tüm kullanıcılar için zorunlu | **Yayınlanmış** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 hibrit anahtar değişimi | **Yayınlanmış** (2024) |
| **X-Wing** (IETF taslağı) | X25519 + ML-KEM-768, amaca yönelik birleştirici | Taslak standart |
| **Zcash** | Kuantum kurtarılabilirlik taslak ZIP (fon kurtarma, şifreleme değil) | Yalnızca tartışma |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (önerilmiş) | Tasarım aşaması |

## Ne Zaman Yayınlanmalı

### Zaman Çizelgesi Sorusu

- **Mevcut durum (2026):** Hiçbir kuantum bilgisayar 255 bit ECC'yi kıramaz. Gösterilmiş en büyük kuantum çarpanlarına ayırma: ~50 bit. Fark: büyüklük sıraları.
- **Yakın vadeli (2030-2035):** IBM, Google, Quantinuum'dan donanım yol haritaları milyonlarca kübiti hedefliyor. ML-KEM uygulamaları ve parametre setleri olgunlaşmış olacak.
- **Orta vadeli (2035-2050):** Çoğu tahmin CRQC varışını bu pencereye koyar. Bugün toplanan HNDL verileri risk altındadır.
- **Uzun vadeli (2050+):** Kriptograflar arasındaki uzlaşma: büyük ölçekli kuantum bilgisayarlar "eğer" değil "ne zaman" meselesidir.

### Önerilen Strateji

**1. Şimdi yükseltilebilirlik için tasarlayın.** Depolanan kayıt formatı, `TransmittedNoteCiphertext` yapısı ve BulkAppendTree giriş düzeni sürümlü ve genişletilebilir olsun. Düşük maliyetlidir ve daha sonra hibrit KEM ekleme seçeneğini korur.

**2. Hazır olduğunda hibrit KEM'i yayınlayın, zorunlu kılın.** İki havuz (klasik ve hibrit) sunmayın. Anonimlik setini bölmek korumalı işlemlerin amacını bozar -- daha küçük bir grupta saklanan kullanıcılar daha az özeldir, daha fazla değil. Yayınlandığında her not hibrit şemayı kullanır.

**3. 2028-2030 penceresini hedefleyin.** Bu, herhangi bir gerçekçi kuantum tehditten çok önce ancak ML-KEM uygulamaları ve parametre boyutları stabilize olduktan sonradır. Ayrıca Zcash ve Signal'in yayınlama deneyiminden öğrenmeye izin verir.

**4. Tetikleyici olayları izleyin:**
- NIST veya NSA'nın kuantum sonrası göç son tarihleri dayatması
- Kuantum donanımında önemli ilerlemeler (hata düzeltme ile >100,000 fiziksel kübit)
- Kafes problemlerine karşı kriptanalitik ilerlemeler (ML-KEM seçimini etkiler)

### Acil Eylem Gerektirmeyen Şey

| Bileşen | Neden bekleyebilir |
|-----------|----------------|
| Harcama yetkilendirme imzaları | Sahtecilik gerçek zamanlıdır, geriye dönük değildir. CRQC gelmeden ML-DSA/SLH-DSA'ya yükselt. |
| Halo 2 ispat sistemi | İspat sahteciliği gerçek zamanlıdır. Gerektiğinde STARK tabanlı sisteme geç. |
| Sinsemilla çarpışma direnci | Yalnızca yeni saldırılar için yararlı, geriye dönük değil. İspat sistemi göçü kapsamında. |
| GroveDB Merk/MMR/Blake3 altyapısı | **Mevcut kriptografik varsayımlar altında zaten kuantum güvenli.** Bilinen saldırılara dayalı olarak herhangi bir eylem gerekli değildir. |

## Kuantum Sonrası Alternatifler Referansı

### Şifreleme İçin (ECDH yerine)

| Şema | Tür | Açık anahtar | Şifre metni | NIST Kategorisi | Notlar |
|--------|------|-----------|-----------|---------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192 bit) | FIPS 203, sektör standardı |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128 bit) | Daha küçük, düşük marj |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256 bit) | Hibrit için aşırı |

### İmzalar İçin (RedPallas/Schnorr yerine)

| Şema | Tür | Açık anahtar | İmza | NIST Kategorisi | Notlar |
|--------|------|-----------|----------|---------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, hızlı |
| SLH-DSA (SPHINCS+) | Hash tabanlı | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, muhafazakâr |
| XMSS/LMS | Hash tabanlı (durumlu) | 60 B | 2,500 B | değişken | Durumlu -- yeniden kullanım = kırılma |

### ZK İspatlar İçin (Halo 2 yerine)

| Sistem | Varsayım | İspat boyutu | Kuantum sonrası | Notlar |
|--------|-----------|-----------|-------------|-------|
| STARKs | Hash fonksiyonları (çarpışma direnci) | ~100-400 KB | **Evet** | StarkNet tarafından kullanılır |
| Plonky3 | FRI (hash tabanlı polinom taahhüt) | ~50-200 KB | **Evet** | Aktif geliştirme |
| Halo 2 (mevcut) | Pasta eğrileri üzerinde ECDLP | ~5 KB | **Hayır** | Mevcut Orchard sistemi |
| Lattice SNARKs | MLWE | Araştırma | **Evet** | Üretime hazır değil |

### Rust Crate Ekosistemi

| Crate | Kaynak | FIPS 203 | Doğrulanmış | Notlar |
|-------|--------|----------|----------|-------|
| `libcrux-ml-kem` | Cryspen | Evet | Resmi olarak doğrulanmış (hax/F*) | En yüksek güvence |
| `ml-kem` | RustCrypto | Evet | Sabit zamanlı, denetlenmemiş | Ekosistem uyumluluğu |
| `fips203` | integritychain | Evet | Sabit zamanlı | Saf Rust, no_std |

## Özet

```text
┌─────────────────────────────────────────────────────────────────────┐
│  GROVEDB + ORCHARD İÇİN KUANTUM TEHDİT ÖZETİ                      │
│                                                                     │
│  MEVCUT VARSAYIMLAR ALTINDA GÜVENLİ (hash tabanlı):                │
│    ✓ Blake3 Merk ağaçları, MMR, BulkAppendTree                     │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ ChaCha20-Poly1305 simetrik şifreleme                          │
│    ✓ Tüm GroveDB kanıt doğrulama zincirleri                        │
│                                                                     │
│  VERİ DEPOLANMADAN ÖNCE DÜZELT (geriye dönük HNDL):               │
│    ✗ Not şifreleme (ECDH anahtar anlaşması) → Hibrit KEM          │
│    ✗ Değer taahhütleri (Pedersen) → tutarlar açığa çıkar           │
│                                                                     │
│  KUANTUM BİLGİSAYARLAR GELMEDEN ÖNCE DÜZELT                        │
│  (yalnızca gerçek zamanlı):                                         │
│    ~ Harcama yetkilendirmesi → ML-DSA / SLH-DSA                   │
│    ~ ZK kanıtları → STARKs / Plonky3                               │
│    ~ Sinsemilla → hash tabanlı Merkle ağacı                        │
│                                                                     │
│  ÖNERİLEN ZAMAN ÇİZELGESİ:                                         │
│    2026-2028: Yükseltilebilirlik için tasarla, formatları versionla │
│    2028-2030: Not şifreleme için zorunlu hibrit KEM yayınla        │
│    2035+: Gerekirse imza ve ispat sistemini taşı                   │
└─────────────────────────────────────────────────────────────────────┘
```

---
