# EvoMu Deep-Dive Spec 02: Canonical Mathematical Formulas Reference (`§3.6` — 38 Mechanics Domains, 161 Equations)

> Part of the [`PLAN.md`](../../PLAN.md) Master Specification.

---

### 3.6 Canonical Mathematical Formulas Reference (Extracted from `~/ai/eve-wiki` — 38 Mechanics Domains, 161 Equations)

All 161 LaTeX equations across 38 mechanics articles in `~/ai/eve-wiki` are cataloged in [`docs/index/wiki_mechanics_catalog.json`](../index/wiki_mechanics_catalog.json). Below is the canonical mathematical specification that `eve-dogma`, `eve-destiny`, and `eve-server` enforce (with per-item parameters read dynamically from the SDE):

1. **Sub-Warp Kinematics, Align Time & Propulsion Thrust (`source: Acceleration.md, Velocity.md, Propulsion_equipment.md`):**
   - Exponential velocity curve:
     $$V(t) = V_{\max} \times \left(1 - e^{-\frac{t \times 10^6}{I \times M}}\right), \qquad t(V) = -I \times M \times 10^{-6} \times \ln\left(1 - \frac{V}{V_{\max}}\right)$$
   - Time to enter warp ($75\%$ of $V_{\max}$, rounded up to the next $1\text{ s}$ server tick):
     $$t_{\text{align}} = \left\lceil -\frac{I \times M}{10^6} \ln(0.25) \right\rceil = \left\lceil 1.386294 \times 10^{-6} \times I \times M \right\rceil$$
   - Afterburner / Microwarpdrive thrust velocity bonus:
     $$V_{\max} = V_{\text{base}} \times \left(1 + V_{\text{bonus}} \times \frac{\text{Thrust}}{\text{Mass}}\right)$$
2. **4-Phase Long & Short Warp Kinematics (`source: Warp.md, Warp_time_calculation.md`):**
   - Let $k = v_{\text{warp}}$ (in $\text{AU/s}$), $j = \min(v_{\text{warp}} / 3,\ 2.0)$, $a = 1\text{ AU} = 149,597,870,700\text{ m}$, and dropout speed $s = \min(100\text{ m/s},\ v_{\text{subwarp}} / 2)$.
   - **Long Warp ($d_{\text{total}} \ge d_{\text{accel}} + d_{\text{decel}} = a + \frac{k \cdot a}{j}$):**
     $$t_{\text{accel}} = \frac{\ln(v_{\text{warp}} / k)}{k}, \qquad t_{\text{cruise}} = \frac{d_{\text{total}} - d_{\text{accel}} - d_{\text{decel}}}{v_{\text{warp}}}, \qquad t_{\text{decel}} = \frac{\ln(v_{\text{warp}} / s)}{j}$$
   - **Short Warp ($d_{\text{total}} < d_{\text{accel}} + d_{\text{decel}}$):** The ship never reaches cruise phase; peak velocity is:
     $$v_{\max} = \frac{d_{\text{total}} \cdot k \cdot j}{k + j}, \qquad t_{\text{total}} = \frac{\ln(v_{\max} / k)}{k} + \frac{\ln(v_{\max} / s)}{j}$$
3. **Capacitor & Passive Shield Recharge Differential Equation (`source: Capacitor.md, Tanking.md`):**
   $$\frac{dC}{dt} = \frac{10 C_{\max}}{T_{\text{rech}}} \left(\sqrt{\frac{C}{C_{\max}}} - \frac{C}{C_{\max}}\right), \qquad C(t_1) = C_{\max} \left(1 + e^{\frac{5(t_0 - t_1)}{T_{\text{rech}}}} \left(\sqrt{\frac{C_0}{C_{\max}}} - 1\right)\right)^2$$
   - Peak recharge occurs at $C / C_{\max} = 0.25$ with rate $\left.\frac{dC}{dt}\right|_{\max} = \frac{2.5 C_{\max}}{T_{\text{rech}}}$.
4. **0-Indexed Stacking Penalty & Resistance Multiplier (`source: Fitting_ships.md, Tanking.md, Stacking_penalties.md, crates/eve-dogma/src/lib.rs`):**
   - Using 0-indexed rank $n \in \{0, 1, 2, \dots\}$ sorted from strongest to weakest effect:
     $$S(n) = 0.5^{\left(0.45 \times n\right)^2} \approx e^{-\left(\frac{n}{2.67}\right)^2} \implies \{100.0\%,\ 86.91\%,\ 57.06\%,\ 28.30\%,\ 10.60\%,\ 3.00\%\}$$
     $$\text{Resist}_{\text{total}} = 1 - \prod_{n=0}^{k-1} \left(1 - R_n \times S(n)\right)$$
5. **Target Locking Time (`source: Targeting.md`):**
   $$t_{\text{lock}} = \frac{40,000\text{ m}}{\text{ScanResolution} \times \operatorname{arsinh}^2(\text{SignatureRadius})}$$
6. **Turret Hit Chance, Damage Quality Roll & Wrecking Shots (`source: Turret_mechanics.md`):**
   $$\text{ChanceToHit} = 0.5^{\left(\frac{\omega_{\text{trans}} \times 40,000\text{ m}}{\text{Tracking} \times \text{SigRadius}}\right)^2 + \left(\frac{\max(0,\ d - \text{Optimal})}{\text{Falloff}}\right)^2}$$
   - Draw uniform $x \in [0, 1)$:
     - If $x \ge \text{ChanceToHit}$: **Miss** ($0$ damage).
     - If $x < \min(0.01,\ \text{ChanceToHit})$ (i.e., $x < 0.01$ and $x < \text{ChanceToHit}$): **Wrecking Shot** ($3.0 \times D_{\text{base}}$).
     - If $0.01 \le x < \text{ChanceToHit}$: **Normal Hit** with damage multiplier $(x + 0.49) \times D_{\text{base}} \in [0.50, 1.49) \times D_{\text{base}}$.
7. **Missile, Carrier Fighter & EDENCOM Vorton Projector Damage (`source: Missile_mechanics.md, Fighters.md, Weapons.md:144`):**
   $$\text{Damage} = D_{\text{base}} \cdot \min\left(1,\ \frac{S}{E},\ \left(\frac{S \cdot V_e}{E \cdot V_t}\right)^{\text{DRF}}\right), \qquad \text{DRF} = \frac{\ln(\text{DamageReduction})}{\ln(\text{DamageSensitivity})}$$
   - **Fighters** multiply this by optimal/falloff factor $0.5^{\left(\frac{\max(0,\ d - \text{Optimal})}{\text{Falloff}}\right)^2}$.
   - **EDENCOM Vorton Projectors** use the Missile damage formula (ignoring angular velocity/tracking) and arc from the primary target to **up to 9 additional targets within $10\text{ km}$ of the primary target** (up to 10 targets hit total, governed by `maxChainTargets` in `typedogma.fsdbinary`; respects CrimeWatch safety settings).
8. **Deathless Breacher Pod Damage-over-Time (`source: Breacher_Pods.md, typedogma.fsdbinary`):**
   - Applies a $40–60\text{s}$ DoT debuff (`Breacher Pod Clone Longevity` $+10\%/\text{lvl}$) dealing raw HP/s:
     $$\text{DPS}_{\text{breacher}} = \min\left(\text{DPS}_{\text{flat}},\ \text{Pct}_{\max} \times (\text{Shield}_{\max} + \text{Armor}_{\max} + \text{Hull}_{\max})\right)$$
   - Oracle values (`Breacher_Pods.md`): Small SCARAB: $\min(200\text{ HP/s},\ 0.6\%\text{ raw HP/s})$; Medium SCARAB: $\min(600\text{ HP/s},\ 0.8\%\text{ raw HP/s})$.
   - Ignores all target resistances, active Assault Damage Controls, Capital Emergency Hull Energizers, and PANIC modules; continues ticking while in warp or tethered; only the single strongest Breacher Pod debuff ticks on a target at once; cleared on dock or stargate jump.
9. **Electronic Countermeasures (ECM) Jam Probability (`source: Electronic_Countermeasures.md, Drone_mechanics.md`):**
   $$P_{\text{jam}} = \frac{\text{ECMStrength}}{\text{TargetSensorStrength}} \times 0.5^{\left(\frac{\max(0,\ d - \text{Optimal})}{\text{Falloff}}\right)^2}, \qquad P_{\text{multi}} = 1 - \prod_{i=1}^{m} \left(1 - P_{\text{jam}, i}\right)$$
10. **3-Rack Overheating, Heat Dissipation & Module Burnout (`source: Overheating.md, typedogma.fsdbinary`):**
    - Rack heat accumulation while $k$ modules in that rack are overloaded:
      $$H(t) = 1 - e^{-t \cdot \text{heatGenerationMultiplier} \cdot \sum_{i=1}^{k} \text{heatAbsorbtionRateModifier}_i}$$
    - Rack heat dissipation when no modules in the rack are generating heat ($1\%/\text{s}$ baseline):
      $$H(t) = H(0) \cdot e^{-t \cdot \text{heatDissipationRate}}$$
    - Probability of module heat damage per cycle at slot distance $d_{\text{slot}}$ within the same rack (`Overheating.md`):
      $$P_{\text{heatDmg}} = H_{\text{rack}} \cdot \left(\frac{\text{Active/Online Modules in Rack}}{\text{Total Slots in Rack}}\right) \cdot \text{heatAttenuation}^{d_{\text{slot}}}$$
11. **Command Bursts & Mining Crystal Volatility (`source: Command_Bursts.md, Crystal_damage.md`):**
    $$\text{BurstStrength} = \text{BaseCharge} \times \left(1 + \frac{\text{Spec}}{100}\right) \left(1 + \frac{\text{Module}}{100}\right) \left(1 + \frac{\text{Ship}}{100}\right) \left(1 + \frac{\text{Mindlink}}{100}\right) \left(1 + \frac{\text{IndyCore}}{100}\right)$$
    $$\text{CrystalLifeCycles} = \frac{\text{CrystalStructureHP}}{\text{VolatilityDamage} \times \text{Volatility}\%}$$
12. **Capital Jump Drives, Black Ops Portals & Ansiblex Jump Bridges (`source: Jump_drives.md, Navigation_structures.md`):**
    $$\text{Isotopes}_{\text{jump}} = D_{\text{LY}} \times F_{\text{base}} \times (1 - 0.1 \cdot \text{JFC}) \times (1 - 0.1 \cdot \text{JF}) \times \prod_{i} \left(1 - \text{JDE}_i \cdot S(i)\right)$$
    $$\text{Isotopes}_{\text{portal}} = \text{Mass}_{\text{kg}} \times \text{PortalConsumptionMassFactor} \times F_{\text{base}} \times \text{SkillMod} \times D_{\text{LY}}$$
    $$\text{LiquidOzone}_{\text{ansiblex}} = \text{Mass}_{\text{kg}} \times D_{\text{LY}} \times 0.000003 + 50$$
13. **Probe Scanning Maximum Deviation (`source: Probe_scanning.md`):**
    $$\text{MaxDeviation} = \text{BaseMaxDeviation} \times \left(\frac{\text{ScanRange}}{\text{BaseScanRange}}\right) \times (1 - 0.05 \cdot \text{Astrometrics}) \times (1 - 0.05 \cdot \text{AstrometricPinpointing})$$
14. **Market Broker Fee, Relist Fee & Sales Tax (`source: Trading.md, Tax.md`):**
    $$\text{BrokerFee}\% = 3\% - 0.3\% \cdot \text{BrokerRelations} - 0.03\% \cdot \text{FactionStanding} - 0.02\% \cdot \text{CorpStanding}$$
    $$\text{RelistFee} = \left(1 - (0.50 + 0.06 \cdot \text{AdvBrokerRelations})\right) \cdot \text{BrokerFee}\% \cdot V_{\text{new}} + \text{BrokerFee}\% \cdot \max(V_{\text{new}} - V_{\text{old}},\ 0)$$
    $$\text{SalesTax}\% = 7.5\% \times (1 - 0.11 \cdot \text{Accounting})$$
15. **Industry EIV, System Cost Index, Invention, Reprocessing, R&D RP & PI Taxes (`source: Manufacturing.md, Research.md, Invention.md, Reprocessing.md, Datacore_farming.md, Planetary_Industry.md`):**
    $$\text{EIV} = \sum_{i} \text{Qty}_i \times \text{AdjustedPrice}_i, \qquad \text{SCI} = \sqrt{\frac{\text{SystemWorkHours}_{28\text{d}}}{\text{UniverseWorkHours}_{28\text{d}}}}$$
    $$\text{JobCost}_{\text{mfg}} = \text{EIV} \times \left((\text{SCI} \times \text{StructureBonus}) + \text{FacilityTax} + \text{SCCSurcharge} + \text{AlphaCloneTax}\right)$$
    $$P_{\text{invention}} = P_{\text{base}} \times \left(1 + \frac{\text{Sci}_1 + \text{Sci}_2}{30} + \frac{\text{Encryption}}{40}\right) \times \left(1 + \frac{\text{DecryptorProbMod}}{100\%}\right)$$
    $$\text{Yield}_{\text{ore}} = (50 + R_m) \times (1 + \text{Sec}) \times (1 + S_m) \times (1 + 0.03 R) \times (1 + 0.02 R_e) \times (1 + 0.02 O_p) \times (1 + I_m)$$
    $$\text{RP/day} = \left(1 + \frac{20 + 5 N + \text{AgentStanding}}{100}\right) \times (\text{ScienceSkill} + \text{AgentLevel})^2$$
    $$\text{POCO ExportFee} = \text{BaseCost} \times \text{TaxRate} \times (1.5\text{ if Command Center}), \qquad \text{ImportFee} = 0.5 \times \text{ExportFee}$$
16. **Standings, Derived Standings, Effective Standings, Mission/FW LP & Security Status (`source: NPC_standings.md, Missions.md, Faction_warfare.md, Security_status.md, Insurance.md`):**
    $$S_{\text{new}} = S_{\text{old}} + (10 - S_{\text{old}}) \times \Delta_+, \qquad S_{\text{new}} = S_{\text{old}} + (-10 - S_{\text{old}}) \times \Delta_-$$
    $$\text{InterFactionMod}_{A \to B} = \text{Standing}_{B \to A} \times \frac{\text{Size}_A}{\text{Size}_B}, \qquad \Delta_{\text{derived}} = \Delta_{\text{primary}} \times \frac{\text{InterFactionMod}_{A \to B}}{10}$$
    $$S_{\text{effective}} = S_{\text{unmodified}} + (10 - S_{\text{unmodified}}) \times (\text{Modifier} \times \text{SkillLevel})$$
    $$\text{LP}_{\text{mission}} = \text{BaseLP} \times (1.6288 - \text{SystemSecurity}) \times (1 + 0.10 \times \text{DivisionConnections})$$
    $$\text{LP}_{\text{FW PvP}} = 3.25 \times \frac{\text{HullValue} - \text{InsurancePayout} + \text{FittedAndCargoValue}}{10,000}$$

---

