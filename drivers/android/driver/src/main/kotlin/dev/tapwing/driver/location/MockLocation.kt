// Copyright (c) 2022 mobile.dev inc.
// Modifications copyright (c) 2026 the Tapwing authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
// Ported from Maestro 2.12.1 (github.com/mobile-dev-inc/maestro, tag cli-2.12.1),
// maestro-android/src/androidTest/java/dev/mobile/maestro/location/
// (MockLocationProvider.kt, LocationManagerProvider.kt, FusedLocationProvider.kt,
// PlayServices.kt) and `Service.createMockProviders` in MaestroDriverService.kt.
// Changes: one file; provider creation moved here from the service.
package dev.tapwing.driver.location

import android.content.Context
import android.location.Location
import android.location.LocationManager
import android.os.Build
import com.google.android.gms.common.ConnectionResult
import com.google.android.gms.common.GoogleApiAvailability
import com.google.android.gms.location.FusedLocationProviderClient
import com.google.android.gms.location.LocationServices

interface MockLocationProvider {
    fun setLocation(location: Location)
    fun enable()
    fun disable()
    fun getProviderName(): String
}

class LocationManagerProvider(
    private val locationManager: LocationManager,
    private val name: String,
    private val requiresNetwork: Boolean,
    private val requiresCell: Boolean,
    private val requiresSatellite: Boolean,
    private val hasMonetaryCost: Boolean,
    private val supportsAltitude: Boolean,
    private val supportsSpeed: Boolean,
    private val supportsBearing: Boolean,
    private val powerRequirement: Int,
    private val accuracy: Int,
) : MockLocationProvider {

    override fun setLocation(location: Location) {
        locationManager.setTestProviderLocation(name, location)
    }

    override fun enable() {
        // Maestro passes the cell and satellite flags in this (swapped) order; kept for parity.
        @Suppress("DEPRECATION")
        locationManager.addTestProvider(
            name, requiresNetwork, requiresSatellite, requiresCell, hasMonetaryCost,
            supportsAltitude, supportsSpeed, supportsBearing, powerRequirement, accuracy,
        )
        locationManager.setTestProviderEnabled(name, true)
    }

    override fun disable() {
        locationManager.setTestProviderEnabled(name, false)
        locationManager.removeTestProvider(name)
    }

    override fun getProviderName(): String = name
}

class FusedLocationProvider(private val client: FusedLocationProviderClient) : MockLocationProvider {
    override fun setLocation(location: Location) {
        client.setMockLocation(location)
    }

    override fun enable() {
        client.setMockMode(true)
    }

    override fun disable() {
        client.setMockMode(false)
    }

    override fun getProviderName(): String = "fused"
}

object MockProviders {

    /** Every non-passive `LocationManager` provider, plus the fused provider when Play services exist. */
    fun create(context: Context, locationManager: LocationManager): List<MockLocationProvider> {
        val fused = if (playServicesAvailable(context)) {
            FusedLocationProvider(LocationServices.getFusedLocationProviderClient(context))
        } else {
            null
        }
        return (
            locationManager.allProviders.mapNotNull {
                if (it == LocationManager.PASSIVE_PROVIDER) null else createLocationManagerProvider(locationManager, it)
            } + fused
            ).filterNotNull()
    }

    private fun playServicesAvailable(context: Context): Boolean =
        GoogleApiAvailability.getInstance().isGooglePlayServicesAvailable(context) == ConnectionResult.SUCCESS

    @Suppress("DEPRECATION")
    private fun createLocationManagerProvider(locationManager: LocationManager, providerName: String?): MockLocationProvider? {
        if (providerName == null) return null
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val p = locationManager.getProviderProperties(providerName) ?: return null
            return LocationManagerProvider(
                locationManager, providerName, p.hasNetworkRequirement(), p.hasSatelliteRequirement(),
                p.hasCellRequirement(), p.hasMonetaryCost(), p.hasAltitudeSupport(), p.hasSpeedSupport(),
                p.hasBearingSupport(), p.powerUsage, p.accuracy,
            )
        }
        val provider = locationManager.getProvider(providerName) ?: return null
        return LocationManagerProvider(
            locationManager, provider.name, provider.requiresNetwork(), provider.requiresSatellite(),
            provider.requiresCell(), provider.hasMonetaryCost(), provider.supportsAltitude(),
            provider.supportsSpeed(), provider.supportsBearing(), provider.powerRequirement, provider.accuracy,
        )
    }
}
